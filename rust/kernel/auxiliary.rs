// SPDX-License-Identifier: GPL-2.0

//! Abstractions for the auxiliary bus.
//!
//! C header: [`include/linux/auxiliary_bus.h`](srctree/include/linux/auxiliary_bus.h)

use crate::{
    bindings,
    container_of,
    device,
    device_id::{
        RawDeviceId,
        RawDeviceIdIndex, //
    },
    devres::Devres,
    driver,
    error::{
        from_result,
        to_result, //
    },
    prelude::*,
    sync::aref::ARef,
    types::{
        ForLt,
        ForeignOwnable,
        Opaque, //
    },
    ThisModule, //
};
use core::{
    any::TypeId,
    marker::PhantomData,
    mem::offset_of,
    pin::Pin,
    ptr::{
        addr_of_mut,
        NonNull, //
    },
};

/// An adapter for the registration of auxiliary drivers.
///
/// `F` is a [`ForLt`](trait@ForLt) type that maps lifetimes to the driver's device
/// private data type, i.e. `F::Of<'bound>` is the driver struct
/// parameterized by `'bound`. The macro `module_auxiliary_driver!`
/// generates this automatically via `ForLt!()`.
pub struct Adapter<F>(PhantomData<F>);

// SAFETY:
// - `bindings::auxiliary_driver` is a C type declared as `repr(C)`.
// - `F::Of<'static>` is the stored type of the driver's device private data.
// - `struct auxiliary_driver` embeds a `struct device_driver`.
// - `DEVICE_DRIVER_OFFSET` is the correct byte offset to the embedded `struct device_driver`.
unsafe impl<F> driver::DriverLayout for Adapter<F>
where
    F: ForLt + 'static,
    for<'bound> F::Of<'bound>: Driver<'bound>,
{
    type DriverType = bindings::auxiliary_driver;
    type DriverData = F;
    const DEVICE_DRIVER_OFFSET: usize = core::mem::offset_of!(Self::DriverType, driver);
}

// SAFETY: A call to `unregister` for a given instance of `DriverType` is guaranteed to be valid if
// a preceding call to `register` has been successful.
unsafe impl<F> driver::RegistrationOps for Adapter<F>
where
    F: ForLt + 'static,
    for<'bound> F::Of<'bound>: Driver<'bound>,
{
    unsafe fn register(
        adrv: &Opaque<Self::DriverType>,
        name: &'static CStr,
        module: &'static ThisModule,
    ) -> Result {
        // SAFETY: It's safe to set the fields of `struct auxiliary_driver` on initialization.
        unsafe {
            (*adrv.get()).name = name.as_char_ptr();
            (*adrv.get()).probe = Some(Self::probe_callback);
            (*adrv.get()).remove = Some(Self::remove_callback);
            (*adrv.get()).id_table = <F::Of<'static> as Driver<'static>>::ID_TABLE.as_ptr();
        }

        // SAFETY: `adrv` is guaranteed to be a valid `DriverType`.
        to_result(unsafe {
            bindings::__auxiliary_driver_register(adrv.get(), module.0, name.as_char_ptr())
        })
    }

    unsafe fn unregister(adrv: &Opaque<Self::DriverType>) {
        // SAFETY: `adrv` is guaranteed to be a valid `DriverType`.
        unsafe { bindings::auxiliary_driver_unregister(adrv.get()) }
    }
}

impl<F> Adapter<F>
where
    F: ForLt + 'static,
    for<'bound> F::Of<'bound>: Driver<'bound>,
{
    extern "C" fn probe_callback(
        adev: *mut bindings::auxiliary_device,
        id: *const bindings::auxiliary_device_id,
    ) -> c_int {
        type RegDataForLt<F> = <<F as ForLt>::Of<'static> as Driver<'static>>::RegistrationData;
        type RegData<F> = <RegDataForLt<F> as ForLt>::Of<'static>;

        // Validate that the stored registration data matches the type the driver expects. This
        // runs once at probe time, allowing `Device::registration_data()` to be infallible.
        //
        // SAFETY: `adev` is a valid pointer to a `struct auxiliary_device`.
        let reg_ptr = unsafe { (*adev).registration_data_rust };
        if TypeId::of::<RegData<F>>() != TypeId::of::<()>() {
            if reg_ptr.is_null() {
                return ENODEV.to_errno();
            }

            // SAFETY: `reg_ptr` is non-null; `RegistrationData` is `#[repr(C)]` with
            // `type_id` at offset 0, so reading a `TypeId` is valid regardless of `F`.
            let stored = unsafe { reg_ptr.cast::<TypeId>().read() };
            if stored != TypeId::of::<RegData<F>>() {
                return ENODEV.to_errno();
            }
        }

        // SAFETY: The auxiliary bus only ever calls the probe callback with a valid pointer to a
        // `struct auxiliary_device`. `Device` is covariant in `R`, so storing the `'static`
        // version of the registration data type is sound — the compiler narrows it through
        // subtyping when `probe()` borrows the device.
        //
        // INVARIANT: `adev` is valid for the duration of `probe_callback()`.
        let adev = unsafe { &*adev.cast::<Device<device::CoreInternal, RegData<F>>>() };

        // SAFETY: `DeviceId` is a `#[repr(transparent)`] wrapper of `struct auxiliary_device_id`
        // and does not add additional invariants, so it's safe to transmute.
        let id = unsafe { &*id.cast::<DeviceId>() };

        from_result(|| {
            let info = <F::Of<'_> as Driver<'_>>::ID_TABLE.info(id.index());
            let data = <F::Of<'_> as Driver<'_>>::probe(adev, info);

            adev.as_ref().set_drvdata::<F>(data)?;
            Ok(0)
        })
    }

    extern "C" fn remove_callback(adev: *mut bindings::auxiliary_device) {
        type RegDataForLt<F> = <<F as ForLt>::Of<'static> as Driver<'static>>::RegistrationData;
        type RegData<F> = <RegDataForLt<F> as ForLt>::Of<'static>;

        // SAFETY: The auxiliary bus only ever calls the probe callback with a valid pointer to a
        // `struct auxiliary_device`.
        //
        // INVARIANT: `adev` is valid for the duration of `remove_callback()`.
        let adev = unsafe { &*adev.cast::<Device<device::CoreInternal, RegData<F>>>() };

        // SAFETY: `remove_callback` is only ever called after a successful call to
        // `probe_callback`, hence it's guaranteed that drvdata has been set.
        let data = unsafe { adev.as_ref().drvdata_borrow::<F>() };

        <F::Of<'_> as Driver<'_>>::unbind(adev, data);
    }
}

/// Declares a kernel module that exposes a single auxiliary driver.
#[macro_export]
macro_rules! module_auxiliary_driver {
    (type: $type:ty, $($rest:tt)*) => {
        $crate::module_driver!(<T>, $crate::auxiliary::Adapter<T>, {
            type: $crate::types::ForLt!($type),
            $($rest)*
        });
    };
}

/// Abstraction for `bindings::auxiliary_device_id`.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct DeviceId(bindings::auxiliary_device_id);

impl DeviceId {
    /// Create a new [`DeviceId`] from name.
    pub const fn new(modname: &'static CStr, name: &'static CStr) -> Self {
        let name = name.to_bytes_with_nul();
        let modname = modname.to_bytes_with_nul();

        let mut id: bindings::auxiliary_device_id = pin_init::zeroed();
        let mut i = 0;
        while i < modname.len() {
            id.name[i] = modname[i];
            i += 1;
        }

        // Reuse the space of the NULL terminator.
        id.name[i - 1] = b'.';

        let mut j = 0;
        while j < name.len() {
            id.name[i] = name[j];
            i += 1;
            j += 1;
        }

        Self(id)
    }
}

// SAFETY: `DeviceId` is a `#[repr(transparent)]` wrapper of `auxiliary_device_id` and does not add
// additional invariants, so it's safe to transmute to `RawType`.
unsafe impl RawDeviceId for DeviceId {
    type RawType = bindings::auxiliary_device_id;
}

// SAFETY: `DRIVER_DATA_OFFSET` is the offset to the `driver_data` field.
unsafe impl RawDeviceIdIndex for DeviceId {
    const DRIVER_DATA_OFFSET: usize =
        core::mem::offset_of!(bindings::auxiliary_device_id, driver_data);

    fn index(&self) -> usize {
        self.0.driver_data
    }
}

/// IdTable type for auxiliary drivers.
pub type IdTable<T> = &'static dyn kernel::device_id::IdTable<DeviceId, T>;

/// Create a auxiliary `IdTable` with its alias for modpost.
#[macro_export]
macro_rules! auxiliary_device_table {
    ($table_name:ident, $module_table_name:ident, $id_info_type: ty, $table_data: expr) => {
        const $table_name: $crate::device_id::IdArray<
            $crate::auxiliary::DeviceId,
            $id_info_type,
            { $table_data.len() },
        > = $crate::device_id::IdArray::new($table_data);

        $crate::module_device_table!("auxiliary", $module_table_name, $table_name);
    };
}

/// The auxiliary driver trait.
///
/// Drivers must implement this trait in order to get an auxiliary driver registered.
pub trait Driver<'bound>: Send {
    /// The type holding information about each device id supported by the driver.
    ///
    /// TODO: Use associated_type_defaults once stabilized:
    ///
    /// type IdInfo: 'static = ();
    type IdInfo: 'static;

    /// The [`ForLt`](trait@ForLt) encoding of the registration data type set
    /// by the parent driver. The framework validates this against the stored
    /// [`TypeId`] before calling [`Driver::probe`]; if the type does not
    /// match, the device is not bound to the driver.
    ///
    /// TODO: Use associated_type_defaults once stabilized:
    ///
    /// type RegistrationData: ForLt = ForLt!(());
    type RegistrationData: ForLt;

    /// The table of device ids supported by the driver.
    const ID_TABLE: IdTable<Self::IdInfo>;

    /// Auxiliary driver probe.
    ///
    /// Called when an auxiliary device matches this driver and the registration
    /// data type has been validated.
    fn probe(
        dev: &'bound Device<device::Core, <Self::RegistrationData as ForLt>::Of<'bound>>,
        id_info: &'bound Self::IdInfo,
    ) -> impl PinInit<Self, Error> + 'bound;

    /// Auxiliary driver unbind.
    ///
    /// Called when a [`Device`] is unbound from its bound [`Driver`]. Implementing this callback
    /// is optional.
    ///
    /// This callback serves as a place for drivers to perform teardown operations that require a
    /// `&Device<Core>` or `&Device<Bound>` reference. For instance, drivers may try to perform I/O
    /// operations to gracefully tear down the device.
    ///
    /// Otherwise, release operations for driver resources should be performed in `Self::drop`.
    fn unbind(
        dev: &'bound Device<device::Core, <Self::RegistrationData as ForLt>::Of<'bound>>,
        this: Pin<&'bound Self>,
    ) {
        let _ = (dev, this);
    }
}

/// The auxiliary device representation.
///
/// This structure represents the Rust abstraction for a C `struct auxiliary_device`. The
/// implementation abstracts the usage of an already existing C `struct auxiliary_device` within
/// Rust code that we get passed from the C side.
///
/// The type parameter `R` is the concrete registration data type set by the
/// parent driver. The framework sets `R` to the `'static`-erased variant at
/// probe time after validating the [`TypeId`]; `Device` is covariant in `R`,
/// so borrows naturally shorten any lifetime in `R` to the reference's
/// lifetime.
///
/// # Invariants
///
/// A [`Device`] instance represents a valid `struct auxiliary_device` created by the C portion of
/// the kernel.
#[repr(transparent)]
pub struct Device<Ctx: device::DeviceContext = device::Normal, R = ()>(
    Opaque<bindings::auxiliary_device>,
    PhantomData<(Ctx, R)>,
);

impl<Ctx: device::DeviceContext, R> Device<Ctx, R> {
    fn as_raw(&self) -> *mut bindings::auxiliary_device {
        self.0.get()
    }

    /// Returns the auxiliary device' id.
    pub fn id(&self) -> u32 {
        // SAFETY: By the type invariant `self.as_raw()` is a valid pointer to a
        // `struct auxiliary_device`.
        unsafe { (*self.as_raw()).id }
    }

    /// Erases the registration data type parameter, returning a reference to
    /// the untyped device.
    pub fn as_untyped(&self) -> &Device<Ctx> {
        // SAFETY: `Device` is `#[repr(transparent)]`; `R` only appears in
        // `PhantomData` and does not affect layout.
        unsafe { &*core::ptr::from_ref(self).cast() }
    }
}

impl<R> Device<device::Bound, R> {
    /// Returns a bound reference to the parent [`device::Device`].
    pub fn parent(&self) -> &device::Device<device::Bound> {
        let parent = (**self).parent();

        // SAFETY: A bound auxiliary device always has a bound parent device.
        unsafe { parent.as_bound() }
    }
}

impl<R> Device<device::Bound, R> {
    /// Returns a pinned reference to the registration data set by the
    /// registering (parent) driver.
    ///
    /// This accessor is infallible: the framework validates the registration
    /// data [`TypeId`] during probe, so the type is guaranteed to match `R`.
    ///
    /// `Device` is covariant in `R`, so the borrow naturally shortens any
    /// lifetime in `R` from `'static` (storage) to the device reference's
    /// lifetime.
    pub fn registration_data(&self) -> Pin<&R> {
        // SAFETY: By the type invariant, `self.as_raw()` is a valid `struct auxiliary_device`.
        // The probe callback validated the `TypeId` and confirmed a non-null pointer.
        let ptr = unsafe { (*self.as_raw()).registration_data_rust };

        // SAFETY: `ptr` was validated during probe and points to a valid
        // `RegistrationData<_>`. The stored data is the `'static` version of `R`;
        // lifetimes do not affect layout, so the cast is valid. `data` is a
        // structurally pinned field of `RegistrationData`.
        unsafe {
            let data = &(*ptr.cast::<RegistrationData<R>>()).data;
            Pin::new_unchecked(data)
        }
    }
}

impl<R> Device<device::Normal, R> {
    /// Returns a reference to the parent [`device::Device`].
    pub fn parent(&self) -> &device::Device {
        // SAFETY: A `struct auxiliary_device` always has a parent.
        unsafe { self.as_ref().parent().unwrap_unchecked() }
    }
}

impl Device {
    extern "C" fn release(dev: *mut bindings::device) {
        // SAFETY: By the type invariant `self.0.as_raw` is a pointer to the `struct device`
        // embedded in `struct auxiliary_device`.
        let adev = unsafe { container_of!(dev, bindings::auxiliary_device, dev) };

        // SAFETY: `adev` points to the memory that has been allocated in `Registration::new`, via
        // `KBox::new(Opaque::<bindings::auxiliary_device>::zeroed(), GFP_KERNEL)`.
        let _ = unsafe { KBox::<Opaque<bindings::auxiliary_device>>::from_raw(adev.cast()) };
    }
}

// SAFETY: `auxiliary::Device` is a transparent wrapper of `struct auxiliary_device`.
// The offset is guaranteed to point to a valid device field inside `auxiliary::Device`.
unsafe impl<Ctx: device::DeviceContext, R> device::AsBusDevice<Ctx> for Device<Ctx, R> {
    const OFFSET: usize = offset_of!(bindings::auxiliary_device, dev);
}

// `Device` is a `#[repr(transparent)]` wrapper of `Opaque<auxiliary_device>` with phantom data;
// `Ctx` and `R` do not affect the layout. The deref impls below mirror what
// `impl_device_context_deref!` generates but are generic over `R`.
macro_rules! impl_aux_device_context_deref {
    ($src:ty => $dst:ty) => {
        impl<R> ::core::ops::Deref for Device<$src, R> {
            type Target = Device<$dst, R>;

            fn deref(&self) -> &Self::Target {
                let ptr: *const Self = self;

                // CAST: `Device<$src, R>` and `Device<$dst, R>` transparently wrap the same
                // type; `$src`, `$dst`, and `R` are all zero-sized.
                let ptr = ptr.cast::<Self::Target>();

                // SAFETY: `ptr` was derived from `&self`.
                unsafe { &*ptr }
            }
        }
    };
}

impl_aux_device_context_deref!(device::CoreInternal => device::Core);
impl_aux_device_context_deref!(device::Core => device::Bound);
impl_aux_device_context_deref!(device::Bound => device::Normal);

impl<R> From<&Device<device::CoreInternal, R>> for ARef<Device> {
    fn from(dev: &Device<device::CoreInternal, R>) -> Self {
        dev.as_untyped().into()
    }
}

impl<R> From<&Device<device::Core, R>> for ARef<Device> {
    fn from(dev: &Device<device::Core, R>) -> Self {
        dev.as_untyped().into()
    }
}

impl<R> From<&Device<device::Bound, R>> for ARef<Device> {
    fn from(dev: &Device<device::Bound, R>) -> Self {
        dev.as_untyped().into()
    }
}

// SAFETY: Instances of `Device` are always reference-counted.
unsafe impl<R> crate::sync::aref::AlwaysRefCounted for Device<device::Normal, R> {
    fn inc_ref(&self) {
        // SAFETY: The existence of a shared reference guarantees that the refcount is non-zero.
        unsafe { bindings::get_device(self.as_ref().as_raw()) };
    }

    unsafe fn dec_ref(obj: NonNull<Self>) {
        // CAST: `Self` a transparent wrapper of `bindings::auxiliary_device`.
        let adev: *mut bindings::auxiliary_device = obj.cast().as_ptr();

        // SAFETY: By the type invariant of `Self`, `adev` is a pointer to a valid
        // `struct auxiliary_device`.
        let dev = unsafe { addr_of_mut!((*adev).dev) };

        // SAFETY: The safety requirements guarantee that the refcount is non-zero.
        unsafe { bindings::put_device(dev) }
    }
}

impl<Ctx: device::DeviceContext, R> AsRef<device::Device<Ctx>> for Device<Ctx, R> {
    fn as_ref(&self) -> &device::Device<Ctx> {
        // SAFETY: By the type invariant of `Self`, `self.as_raw()` is a pointer to a valid
        // `struct auxiliary_device`.
        let dev = unsafe { addr_of_mut!((*self.as_raw()).dev) };

        // SAFETY: `dev` points to a valid `struct device`.
        unsafe { device::Device::from_raw(dev) }
    }
}

// SAFETY: A `Device` is always reference-counted and can be released from any thread.
unsafe impl<R> Send for Device<device::Normal, R> {}

// SAFETY: `Device` can be shared among threads because all methods of `Device`
// (i.e. `Device<Normal>) are thread safe.
unsafe impl<R> Sync for Device<device::Normal, R> {}

// SAFETY: Same as `Device<Normal>` -- the underlying `struct auxiliary_device` is the same;
// `Bound` is a zero-sized type-state marker that does not affect thread safety.
unsafe impl<R> Sync for Device<device::Bound, R> {}

/// Wrapper that stores a [`TypeId`] alongside the registration data for runtime type checking.
#[repr(C)]
#[pin_data]
struct RegistrationData<T> {
    type_id: TypeId,
    #[pin]
    data: T,
}

/// The registration of an auxiliary device.
///
/// This type represents the registration of a [`struct auxiliary_device`]. When its parent device
/// is unbound, the corresponding auxiliary device will be unregistered from the system.
///
/// The type parameter `F` is a [`ForLt`](trait@ForLt) encoding of the registration
/// data type. For non-lifetime-parameterized types, use [`ForLt!(T)`](macro@ForLt).
/// The data can be accessed by the auxiliary driver through [`Device::registration_data()`].
///
/// # Invariants
///
/// `self.adev` always holds a valid pointer to an initialized and registered
/// [`struct auxiliary_device`] whose `registration_data_rust` field points to a
/// valid `Pin<KBox<RegistrationData<F::Of<'static>>>>`.
pub struct Registration<F: ForLt> {
    adev: NonNull<bindings::auxiliary_device>,
    _data: PhantomData<F>,
}

impl<F: ForLt> Registration<F>
where
    for<'a> F::Of<'a>: Send,
{
    /// Create and register a new auxiliary device with the given registration data.
    ///
    /// The `data` is owned by the registration and can be accessed through the auxiliary device
    /// via [`Device::registration_data()`].
    pub fn new<'bound, E>(
        parent: &'bound device::Device<device::Bound>,
        name: &CStr,
        id: u32,
        modname: &CStr,
        data: impl PinInit<F::Of<'bound>, E>,
    ) -> Result<Devres<Self>>
    where
        Error: From<E>,
    {
        let data = KBox::pin_init::<Error>(
            try_pin_init!(RegistrationData {
                type_id: TypeId::of::<F::Of<'static>>(),
                data <- data,
            }),
            GFP_KERNEL,
        )?;

        // SAFETY: Lifetimes are erased and do not affect layout, so RegistrationData<F::Of<'bound>>
        // and RegistrationData<F::Of<'static>> have identical representation.
        let data: Pin<KBox<RegistrationData<F::Of<'static>>>> =
            unsafe { core::mem::transmute(data) };

        let boxed: KBox<Opaque<bindings::auxiliary_device>> = KBox::zeroed(GFP_KERNEL)?;
        let adev = boxed.get();

        // SAFETY: It's safe to set the fields of `struct auxiliary_device` on initialization.
        unsafe {
            (*adev).dev.parent = parent.as_raw();
            (*adev).dev.release = Some(Device::release);
            (*adev).name = name.as_char_ptr();
            (*adev).id = id;
            (*adev).registration_data_rust = data.into_foreign();
        }

        // SAFETY: `adev` is guaranteed to be a valid pointer to a `struct auxiliary_device`,
        // which has not been initialized yet.
        unsafe { bindings::auxiliary_device_init(adev) };

        // Now that `adev` is initialized, leak the `Box`; the corresponding memory will be
        // freed by `Device::release` when the last reference to the `struct auxiliary_device`
        // is dropped.
        let _ = KBox::into_raw(boxed);

        // SAFETY:
        // - `adev` is guaranteed to be a valid pointer to a `struct auxiliary_device`, which
        //   has been initialized,
        // - `modname.as_char_ptr()` is a NULL terminated string.
        let ret = unsafe { bindings::__auxiliary_device_add(adev, modname.as_char_ptr()) };
        if ret != 0 {
            // SAFETY: `registration_data` was set above via `into_foreign()`.
            drop(unsafe {
                Pin::<KBox<RegistrationData<F::Of<'static>>>>::from_foreign(
                    (*adev).registration_data_rust,
                )
            });

            // SAFETY: `adev` is guaranteed to be a valid pointer to a
            // `struct auxiliary_device`, which has been initialized.
            unsafe { bindings::auxiliary_device_uninit(adev) };

            return Err(Error::from_errno(ret));
        }

        // INVARIANT: The device will remain registered until `auxiliary_device_delete()` is
        // called, which happens in `Self::drop()`.
        let reg = Self {
            // SAFETY: `adev` is guaranteed to be non-null, since the `KBox` was allocated
            // successfully.
            adev: unsafe { NonNull::new_unchecked(adev) },
            _data: PhantomData,
        };

        Devres::new::<core::convert::Infallible>(parent, reg)
    }
}

impl<F: ForLt> Drop for Registration<F> {
    fn drop(&mut self) {
        // SAFETY: By the type invariant of `Self`, `self.adev.as_ptr()` is a valid registered
        // `struct auxiliary_device`.
        unsafe { bindings::auxiliary_device_delete(self.adev.as_ptr()) };

        // SAFETY: `registration_data` was set in `new()` via `into_foreign()`.
        drop(unsafe {
            Pin::<KBox<RegistrationData<F::Of<'static>>>>::from_foreign(
                (*self.adev.as_ptr()).registration_data_rust,
            )
        });

        // This drops the reference we acquired through `auxiliary_device_init()`.
        //
        // SAFETY: By the type invariant of `Self`, `self.adev.as_ptr()` is a valid registered
        // `struct auxiliary_device`.
        unsafe { bindings::auxiliary_device_uninit(self.adev.as_ptr()) };
    }
}

// SAFETY: A `Registration` of a `struct auxiliary_device` can be released from any thread.
unsafe impl<F: ForLt> Send for Registration<F> where for<'a> F::Of<'a>: Send {}

// SAFETY: `Registration` does not expose any methods or fields that need synchronization.
unsafe impl<F: ForLt> Sync for Registration<F> where for<'a> F::Of<'a>: Send {}
