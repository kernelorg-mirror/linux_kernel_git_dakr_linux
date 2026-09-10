// SPDX-License-Identifier: GPL-2.0

//! Abstractions for the PCI bus.
//!
//! C header: [`include/linux/pci.h`](srctree/include/linux/pci.h)

use crate::{
    bindings,
    container_of,
    device,
    device_id::{
        RawDeviceId,
        RawDeviceIdIndex, //
    },
    driver,
    error::{
        from_result,
        to_result, //
    },
    io::resource,
    prelude::*,
    str::CStr,
    types::{
        ForLt,
        Opaque, //
    },
    ThisModule, //
};
use core::{
    any::TypeId,
    marker::{
        PhantomData,
        PhantomPinned, //
    },
    mem::offset_of,
    num::NonZero,
    pin::Pin,
    ptr::{
        addr_of_mut,
        NonNull, //
    },
};

mod id;
mod io;
mod irq;

pub use self::id::{
    Class,
    ClassMask,
    Vendor, //
};
pub use self::io::{
    Bar,
    ConfigSpace,
    ConfigSpaceSize,
    DevresBar,
    Extended,
    Normal, //
};
pub use self::irq::{
    IrqType,
    IrqTypes,
    IrqVector,
    IrqVectorRegistration, //
};

/// An adapter for the registration of PCI drivers.
pub struct Adapter<T: Driver>(T);

// SAFETY:
// - `bindings::pci_driver` is a C type declared as `repr(C)`.
// - `T::Data` is the type of the driver's device private data.
// - `struct pci_driver` embeds a `struct device_driver`.
// - `DEVICE_DRIVER_OFFSET` is the correct byte offset to the embedded `struct device_driver`.
unsafe impl<T: Driver> driver::DriverLayout for Adapter<T> {
    type DriverType = bindings::pci_driver;
    type DriverData<'bound> = T::Data<'bound>;
    const DEVICE_DRIVER_OFFSET: usize = core::mem::offset_of!(Self::DriverType, driver);
}

// SAFETY: A call to `unregister` for a given instance of `DriverType` is guaranteed to be valid if
// a preceding call to `register` has been successful.
unsafe impl<T: Driver> driver::RegistrationOps for Adapter<T> {
    unsafe fn register(
        pdrv: &Opaque<Self::DriverType>,
        name: &'static CStr,
        module: &'static ThisModule,
    ) -> Result {
        // SAFETY: It's safe to set the fields of `struct pci_driver` on initialization.
        unsafe {
            (*pdrv.get()).name = name.as_char_ptr();
            (*pdrv.get()).probe = Some(Self::probe_callback);
            (*pdrv.get()).remove = Some(Self::remove_callback);
            (*pdrv.get()).id_table = T::ID_TABLE.as_ptr();
            (*pdrv.get()).driver_managed_dma = T::DRIVER_MANAGED_DMA;
        }

        // SAFETY: `pdrv` is guaranteed to be a valid `DriverType`.
        to_result(unsafe {
            bindings::__pci_register_driver(pdrv.get(), module.as_ptr(), name.as_char_ptr())
        })
    }

    unsafe fn unregister(pdrv: &Opaque<Self::DriverType>) {
        // SAFETY: `pdrv` is guaranteed to be a valid `DriverType`.
        unsafe { bindings::pci_unregister_driver(pdrv.get()) }
    }
}

impl<T: Driver> Adapter<T> {
    extern "C" fn probe_callback(
        pdev: *mut bindings::pci_dev,
        id: *const bindings::pci_device_id,
    ) -> c_int {
        // SAFETY: The PCI bus only ever calls the probe callback with a valid pointer to a
        // `struct pci_dev`.
        //
        // INVARIANT: `pdev` is valid for the duration of `probe_callback()`.
        let pdev = unsafe { &*pdev.cast::<Device<device::CoreInternal<'_>>>() };

        // SAFETY: `DeviceId` is a `#[repr(transparent)]` wrapper of `struct pci_device_id` and
        // does not add additional invariants, so it's safe to transmute.
        let id = unsafe { &*id.cast::<DeviceId>() };

        // SAFETY: `id` comes from `T::ID_TABLE` which is of type `IdArray<_, T::IdInfo>` or
        // `pci_device_id_any` which has 0 as driver_data. It can also come from dynamic IDs, which
        // will ensure that `driver_data` exists in `T::ID_TABLE`.
        let info = unsafe { id.info_unchecked_opt::<T::IdInfo>() };

        from_result(|| {
            let data = T::probe(pdev, info);

            pdev.as_ref().set_drvdata(data)?;
            Ok(0)
        })
    }

    extern "C" fn remove_callback(pdev: *mut bindings::pci_dev) {
        // SAFETY: The PCI bus only ever calls the remove callback with a valid pointer to a
        // `struct pci_dev`.
        //
        // INVARIANT: `pdev` is valid for the duration of `remove_callback()`.
        let pdev = unsafe { &*pdev.cast::<Device<device::CoreInternal<'_>>>() };

        // SAFETY: `remove_callback` is only ever called after a successful call to
        // `probe_callback`, hence it's guaranteed that `Device::set_drvdata()` has been called
        // and stored a `Pin<KBox<T::Data<'_>>>`.
        let data = unsafe { pdev.as_ref().drvdata_borrow::<T::Data<'_>>() };

        T::unbind(pdev, data);
    }
}

/// Declares a kernel module that exposes a single PCI driver.
///
/// # Examples
///
///```ignore
/// kernel::module_pci_driver! {
///     type: MyDriver,
///     name: "Module name",
///     authors: ["Author name"],
///     description: "Description",
///     license: "GPL v2",
/// }
///```
#[macro_export]
macro_rules! module_pci_driver {
($($f:tt)*) => {
    $crate::module_driver!(<T>, $crate::pci::Adapter<T>, { $($f)* });
};
}

/// Abstraction for the PCI device ID structure ([`struct pci_device_id`]).
///
/// [`struct pci_device_id`]: https://docs.kernel.org/PCI/pci.html#c.pci_device_id
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct DeviceId(bindings::pci_device_id);

impl DeviceId {
    const PCI_ANY_ID: u32 = !0;

    /// Equivalent to C's `PCI_DEVICE` macro.
    ///
    /// Create a new `pci::DeviceId` from a vendor and device ID.
    #[inline]
    pub const fn from_id(vendor: Vendor, device: u32) -> Self {
        Self(bindings::pci_device_id {
            vendor: vendor.as_raw() as u32,
            device,
            subvendor: DeviceId::PCI_ANY_ID,
            subdevice: DeviceId::PCI_ANY_ID,
            class: 0,
            class_mask: 0,
            driver_data: 0,
            override_only: 0,
        })
    }

    /// Match a class and vendor, requiring a VFIO driver override.
    pub const fn from_class_and_vendor_vfio_override(
        class: Class,
        class_mask: ClassMask,
        vendor: Vendor,
    ) -> Self {
        let mut id = Self::from_class_and_vendor(class, class_mask, vendor);
        id.0.override_only = bindings::PCI_ID_F_VFIO_DRIVER_OVERRIDE;
        id
    }

    /// Equivalent to C's `PCI_DEVICE_CLASS` macro.
    ///
    /// Create a new `pci::DeviceId` from a class number and mask.
    #[inline]
    pub const fn from_class(class: u32, class_mask: u32) -> Self {
        Self(bindings::pci_device_id {
            vendor: DeviceId::PCI_ANY_ID,
            device: DeviceId::PCI_ANY_ID,
            subvendor: DeviceId::PCI_ANY_ID,
            subdevice: DeviceId::PCI_ANY_ID,
            class,
            class_mask,
            driver_data: 0,
            override_only: 0,
        })
    }

    /// Create a new [`DeviceId`] from a class number, mask, and specific vendor.
    ///
    /// This is more targeted than [`DeviceId::from_class`]: in addition to matching by [`Vendor`],
    /// it also matches the PCI [`Class`] (up to the entire 24 bits, depending on the
    /// [`ClassMask`]).
    #[inline]
    pub const fn from_class_and_vendor(
        class: Class,
        class_mask: ClassMask,
        vendor: Vendor,
    ) -> Self {
        Self(bindings::pci_device_id {
            vendor: vendor.as_raw() as u32,
            device: DeviceId::PCI_ANY_ID,
            subvendor: DeviceId::PCI_ANY_ID,
            subdevice: DeviceId::PCI_ANY_ID,
            class: class.as_raw(),
            class_mask: class_mask.as_raw(),
            driver_data: 0,
            override_only: 0,
        })
    }
}

// SAFETY: `DeviceId` is a `#[repr(transparent)]` wrapper of `pci_device_id` and does not add
// additional invariants, so it's safe to transmute to `RawType`.
unsafe impl RawDeviceId for DeviceId {
    type RawType = bindings::pci_device_id;
}

// SAFETY: `DRIVER_DATA_OFFSET` is the offset to the `driver_data` field.
unsafe impl RawDeviceIdIndex for DeviceId {
    const DRIVER_DATA_OFFSET: usize = core::mem::offset_of!(bindings::pci_device_id, driver_data);
}

/// `IdTable` type for PCI.
pub type IdTable<T> = &'static dyn kernel::device_id::IdTable<DeviceId, T>;

/// Create a PCI `IdTable` with its alias for modpost.
#[macro_export]
macro_rules! pci_device_table {
    ($($tt:tt)*) => {
        $crate::module_device_table!("pci", $crate::pci::DeviceId, $($tt)*);
    };
}

/// The PCI driver trait.
///
/// # Examples
///
///```
/// # use kernel::{bindings, device::Core, pci};
///
/// struct MyDriver;
///
/// kernel::pci_device_table!(
///     PCI_TABLE,
///     <MyDriver as pci::Driver>::IdInfo,
///     [
///         (
///             pci::DeviceId::from_id(pci::Vendor::REDHAT, bindings::PCI_ANY_ID as u32),
///             (),
///         )
///     ]
/// );
///
/// impl pci::Driver for MyDriver {
///     type IdInfo = ();
///     type Data<'bound> = Self;
///     const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;
///
///     fn probe<'bound>(
///         _pdev: &'bound pci::Device<Core<'_>>,
///         _id_info: Option<&'bound Self::IdInfo>,
///     ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound {
///         Err(ENODEV)
///     }
/// }
///```
/// Drivers must implement this trait in order to get a PCI driver registered. Please refer to the
/// `Adapter` documentation for an example.
pub trait Driver {
    /// The type holding information about each device id supported by the driver.
    // TODO: Use `associated_type_defaults` once stabilized:
    //
    // ```
    // type IdInfo: 'static = ();
    // ```
    type IdInfo: 'static;

    /// The type of the driver's bus device private data.
    type Data<'bound>: Send + 'bound;

    /// The table of device ids supported by the driver.
    const ID_TABLE: IdTable<Self::IdInfo>;

    /// Whether the driver manages its own DMA domain, as VFIO drivers do.
    const DRIVER_MANAGED_DMA: bool = false;

    /// PCI driver probe.
    ///
    /// Called when a new pci device is added or discovered. Implementers should
    /// attempt to initialize the device here.
    fn probe<'bound>(
        dev: &'bound Device<device::Core<'_>>,
        id_info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound;

    /// PCI driver unbind.
    ///
    /// Called when a [`Device`] is unbound from its bound [`Driver`]. Implementing this callback
    /// is optional.
    ///
    /// This callback serves as a place for drivers to perform teardown operations that require a
    /// `&Device<Core>` or `&Device<Bound>` reference. For instance, drivers may try to perform I/O
    /// operations to gracefully tear down the device.
    ///
    /// Otherwise, release operations for driver resources should be performed in `Drop`.
    fn unbind<'bound>(dev: &'bound Device<device::Core<'_>>, this: Pin<&Self::Data<'bound>>) {
        let _ = (dev, this);
    }
}

/// The PCI device representation.
///
/// This structure represents the Rust abstraction for a C `struct pci_dev`. The implementation
/// abstracts the usage of an already existing C `struct pci_dev` within Rust code that we get
/// passed from the C side.
///
/// # Invariants
///
/// A [`Device`] instance represents a valid `struct pci_dev` created by the C portion of the
/// kernel.
#[repr(transparent)]
pub struct Device<Ctx: device::DeviceContext = device::Normal>(
    Opaque<bindings::pci_dev>,
    PhantomData<Ctx>,
);

impl<Ctx: device::DeviceContext> Device<Ctx> {
    #[inline]
    fn as_raw(&self) -> *mut bindings::pci_dev {
        self.0.get()
    }
}

impl Device {
    /// Returns the PCI vendor ID as [`Vendor`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use kernel::{device::Core, pci::{self, Vendor}, prelude::*};
    /// fn log_device_info(pdev: &pci::Device<Core<'_>>) -> Result {
    ///     // Get an instance of `Vendor`.
    ///     let vendor = pdev.vendor_id();
    ///     dev_info!(
    ///         pdev,
    ///         "Device: Vendor={}, Device=0x{:x}\n",
    ///         vendor,
    ///         pdev.device_id()
    ///     );
    ///     Ok(())
    /// }
    /// ```
    #[inline]
    pub fn vendor_id(&self) -> Vendor {
        // SAFETY: `self.as_raw` is a valid pointer to a `struct pci_dev`.
        let vendor_id = unsafe { (*self.as_raw()).vendor };
        Vendor::from_raw(vendor_id)
    }

    /// Returns the PCI device ID.
    #[inline]
    pub fn device_id(&self) -> u16 {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct pci_dev`.
        unsafe { (*self.as_raw()).device }
    }

    /// Returns the PCI revision ID.
    #[inline]
    pub fn revision_id(&self) -> u8 {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct pci_dev`.
        unsafe { (*self.as_raw()).revision }
    }

    /// Returns the PCI bus device/function.
    #[inline]
    pub fn dev_id(&self) -> u16 {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct pci_dev`.
        unsafe { bindings::pci_dev_id(self.as_raw()) }
    }

    /// Returns the PCI domain number of the bus this device is on.
    #[inline]
    pub fn domain_nr(&self) -> u32 {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct pci_dev`.
        let domain_nr = unsafe { bindings::pci_domain_nr(self.as_raw()) };

        // CAST: The C function returns `int`, but a PCI domain number is always
        // non-negative, so this cast will not lose any information.
        domain_nr as u32
    }

    /// Returns the PCI subsystem vendor ID.
    #[inline]
    pub fn subsystem_vendor_id(&self) -> u16 {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct pci_dev`.
        unsafe { (*self.as_raw()).subsystem_vendor }
    }

    /// Returns the PCI subsystem device ID.
    #[inline]
    pub fn subsystem_device_id(&self) -> u16 {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct pci_dev`.
        unsafe { (*self.as_raw()).subsystem_device }
    }

    /// Returns the start of the given PCI BAR resource.
    pub fn resource_start(&self, bar: u32) -> Result<bindings::resource_size_t> {
        if !Bar::index_is_valid(bar) {
            return Err(EINVAL);
        }

        // SAFETY:
        // - `bar` is a valid bar number, as guaranteed by the above call to `Bar::index_is_valid`,
        // - by its type invariant `self.as_raw` is always a valid pointer to a `struct pci_dev`.
        Ok(unsafe { bindings::pci_resource_start(self.as_raw(), bar.try_into()?) })
    }

    /// Returns the size of the given PCI BAR resource.
    pub fn resource_len(&self, bar: u32) -> Result<bindings::resource_size_t> {
        if !Bar::index_is_valid(bar) {
            return Err(EINVAL);
        }

        // SAFETY:
        // - `bar` is a valid bar number, as guaranteed by the above call to `Bar::index_is_valid`,
        // - by its type invariant `self.as_raw` is always a valid pointer to a `struct pci_dev`.
        Ok(unsafe { bindings::pci_resource_len(self.as_raw(), bar.try_into()?) })
    }

    /// Returns the resource flags (`IORESOURCE_*`) of the given PCI BAR.
    pub fn resource_flags(&self, bar: u32) -> Result<resource::Flags> {
        if !Bar::index_is_valid(bar) {
            return Err(EINVAL);
        }

        // SAFETY:
        // - `bar` is a valid bar number, as guaranteed by the above call to `Bar::index_is_valid`,
        // - by its type invariant `self.as_raw` is always a valid pointer to a `struct pci_dev`.
        let raw = unsafe { bindings::pci_resource_flags(self.as_raw(), bar.try_into()?) };
        Ok(resource::Flags::from_raw(raw))
    }

    /// Returns the PCI class as a `Class` struct.
    #[inline]
    pub fn pci_class(&self) -> Class {
        // SAFETY: `self.as_raw` is a valid pointer to a `struct pci_dev`.
        Class::from_raw(unsafe { (*self.as_raw()).class })
    }
}

/// A guard that keeps the device's I/O and memory resources enabled.
///
/// # Invariants
///
/// The device's enable count was incremented once for this guard; dropping the guard decrements
/// it again.
pub struct DeviceEnableGuard<'a> {
    dev: &'a Device<device::Bound>,
}

impl Drop for DeviceEnableGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.dev.as_raw()` is a valid pointer to a `struct pci_dev`, and by the type
        // invariant this guard holds one increment of the device's enable count.
        unsafe { bindings::pci_disable_device(self.dev.as_raw()) };
    }
}

impl<'a> Device<device::Core<'a>> {
    /// Returns the total number of VFs, or [`None`] if SR-IOV is not available.
    #[inline]
    pub fn sriov_get_totalvfs(&self) -> Option<NonZero<u16>> {
        // SAFETY: `self.as_raw()` is a valid pointer to a `struct pci_dev`.
        let total_vfs = unsafe { bindings::pci_sriov_get_totalvfs(self.as_raw()) };

        // CAST: The C function returns `unsigned int`, but the value originates
        // from TotalVFs/driver_max_VFs (which are defined as `u16`), so this cast
        // cannot truncate.
        NonZero::new(total_vfs as u16)
    }

    /// Enable I/O and memory resources for this device.
    ///
    /// The device stays enabled for the lifetime of the returned guard; dropping the guard
    /// disables the device again. The guard borrows the device's bound scope, so it cannot
    /// outlive the driver binding.
    pub fn enable_device(&self) -> Result<DeviceEnableGuard<'_>> {
        // SAFETY: `self.as_raw` is guaranteed to be a pointer to a valid `struct pci_dev`.
        to_result(unsafe { bindings::pci_enable_device(self.as_raw()) })?;

        // INVARIANT: `pci_enable_device()` succeeded, so the enable count was incremented once.
        Ok(DeviceEnableGuard { dev: self })
    }

    /// Enable bus-mastering for this device.
    #[inline]
    pub fn set_master(&self) {
        // SAFETY: `self.as_raw` is guaranteed to be a pointer to a valid `struct pci_dev`.
        unsafe { bindings::pci_set_master(self.as_raw()) };
    }
}

impl Device<device::Bound> {
    /// Returns `true` if this device is a PCI SR-IOV virtual function (VF).
    #[inline]
    pub fn is_virtfn(&self) -> bool {
        // SAFETY: `self.as_raw()` is a valid pointer to a `struct pci_dev`.
        unsafe { bindings::pci_dev_is_virtfn(self.as_raw()) }
    }

    /// Returns `true` if this device is a PCI SR-IOV physical function (PF).
    #[inline]
    pub fn is_physfn(&self) -> bool {
        // SAFETY: `self.as_raw()` is a valid pointer to a `struct pci_dev`.
        unsafe { bindings::pci_dev_is_physfn(self.as_raw()) }
    }

    /// Returns the PF for this VF, or [`ENODEV`] if this is not a VF.
    ///
    /// The returned reference borrows `self`, so the VF (and hence its PF) remains
    /// valid for the lifetime of the reference. The PF is guaranteed bound while
    /// VFs exist because [`VfRegistration`] calls `pci_disable_sriov()` in its
    /// drop, which blocks until all VF drivers have completed their `remove()`.
    pub fn physfn(&self) -> Result<&Device<device::Bound>> {
        if !self.is_virtfn() {
            return Err(ENODEV);
        }
        // SAFETY: `self.as_raw()` is valid and `pci_physfn` returns the PF
        // pointer when the device is a VF. The PF remains bound because
        // `VfRegistration` owns the SR-IOV lifecycle.
        let pf = unsafe { bindings::pci_physfn(self.as_raw()) };
        if pf.is_null() {
            return Err(ENODEV);
        }
        // SAFETY: `pf` is a valid PCI device pointer whose driver is bound.
        Ok(unsafe { &*pf.cast::<Device<device::Bound>>() })
    }

    /// Returns the VF index (0-based) within the PF, or an error if not a VF.
    pub fn vf_id(&self) -> Result<u32> {
        if !self.is_virtfn() {
            return Err(ENODEV);
        }
        // SAFETY: `self.as_raw()` is a valid VF device.
        let id = unsafe { bindings::pci_iov_vf_id(self.as_raw()) };
        if id < 0 {
            return Err(Error::from_errno(id));
        }
        Ok(id as u32)
    }

    /// Returns the raw `vf_registration_data_rust` pointer from this device.
    fn vf_registration_data_rust(&self) -> *mut core::ffi::c_void {
        // SAFETY: `self.as_raw()` is valid.
        unsafe { (*self.as_raw()).vf_registration_data_rust }
    }

    /// Sets the `vf_registration_data_rust` pointer on this device.
    fn set_vf_registration_data_rust(&self, ptr: *mut core::ffi::c_void) {
        // SAFETY: `self.as_raw()` is valid. This is only called from
        // `VfRegistration` init/drop which serializes access.
        unsafe { (*self.as_raw()).vf_registration_data_rust = ptr };
    }

    /// Access the VF registration data through a closure with an HRTB lifetime.
    ///
    /// `F` is the [`ForLt`](trait@ForLt) encoding of the data type. Returns
    /// [`ENODEV`] if this is not a VF, [`ENOENT`] if no data was registered,
    /// or [`EINVAL`] if `F` does not match the type registered by the PF.
    ///
    /// The pointer is guaranteed valid while this VF is probed, because
    /// [`VfRegistration`] calls `pci_disable_sriov()` in its drop (which
    /// blocks until all VF `remove()` callbacks complete) before clearing the
    /// pointer and dropping the data.
    pub fn vf_registration_data_with<F: ForLt + 'static, R>(
        &self,
        f: impl for<'a> FnOnce(Pin<&F::Of<'a>>) -> R,
    ) -> Result<R> {
        // SAFETY: The HRTB on the closure prevents the caller from smuggling
        // in a concrete short lifetime. See `registration_data_pinned`.
        let pinned = unsafe { self.vf_registration_data_pinned::<F>()? };
        Ok(f(pinned))
    }

    /// Returns a pinned reference to the VF registration data.
    ///
    /// Available only when `F` implements [`CovariantForLt`](trait@crate::types::CovariantForLt),
    /// guaranteeing that the lifetime shortening from `'static` is sound.
    ///
    /// For non-covariant types, use [`Self::vf_registration_data_with()`].
    pub fn vf_registration_data<F: crate::types::CovariantForLt + 'static>(
        &self,
    ) -> Result<Pin<&F::Of<'_>>> {
        // SAFETY: `CovariantForLt` guarantees that the lifetime shortening is
        // sound.
        unsafe { self.vf_registration_data_pinned::<F>() }
    }

    /// Internal helper: reads the `vf_registration_data_rust` pointer from the
    /// PF, checks the `TypeId`, and returns a pinned reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure the returned reference is only used behind an
    /// HRTB closure or with a covariant type.
    unsafe fn vf_registration_data_pinned<F: ForLt + 'static>(&self) -> Result<Pin<&F::Of<'_>>> {
        let pf = self.physfn()?;

        let ptr = pf.vf_registration_data_rust();
        if ptr.is_null() {
            return Err(ENOENT);
        }

        // SAFETY: `ptr` points to a `VfRegistrationData` whose first field is
        // a `TypeId`.
        let type_id = unsafe { ptr.cast::<TypeId>().read() };
        if type_id != TypeId::of::<F>() {
            return Err(EINVAL);
        }

        // SAFETY: TypeId check confirms the stored type matches `F`. The data
        // is pinned inside the PF's driver data struct. Lifetime shortening
        // from the PF's binding scope to `'_` is layout-compatible.
        let data_ptr = unsafe {
            let vfrd = ptr.cast::<VfRegistrationData<'_, F>>();
            &raw const (*vfrd).data
        };
        // SAFETY: `data` is structurally pinned inside `VfRegistrationData`.
        Ok(unsafe { Pin::new_unchecked(&*data_ptr) })
    }
}

/// Wrapper for VF registration data stored inside a [`VfRegistration`].
///
/// Stores a [`TypeId`] header (derived from `F`) followed by the pinned data,
/// so that [`Device::vf_registration_data_with()`] can verify the type at
/// runtime.
#[repr(C)]
#[pin_data]
pub struct VfRegistrationData<'a, F: ForLt + 'static> {
    type_id: TypeId,
    #[pin]
    data: F::Of<'a>,
}

impl<'a, F: ForLt + 'static> VfRegistrationData<'a, F> {
    /// Pin-initializer for the registration data.
    pub fn new(data: impl PinInit<F::Of<'a>, Error>) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            type_id: TypeId::of::<F>(),
            data <- data,
        })
    }
}

/// SR-IOV VF registration on a PF device.
///
/// Owns the SR-IOV enable/disable lifecycle and the registration data that VF
/// drivers access via [`Device::vf_registration_data_with()`] and
/// [`Device::vf_registration_data()`]. The data lives inline (no separate
/// allocation) and is initialized via pin-init.
///
/// The constructor stores a pointer to the inline [`VfRegistrationData`] on
/// `pci_dev.vf_registration_data_rust` and optionally calls
/// `pci_enable_sriov()`. Drop always calls `pci_disable_sriov()` (which blocks
/// until all VF `remove()` callbacks complete) before clearing the pointer and
/// letting the data fields drop.
///
/// Fails if the device is not a PF or if a registration already exists.
#[pin_data(PinnedDrop)]
pub struct VfRegistration<'a, F: ForLt + 'static> {
    pdev: &'a Device<device::Bound>,
    #[pin]
    inner: VfRegistrationData<'a, F>,
    #[pin]
    _pin: PhantomPinned,
}

impl<'a, F: ForLt + 'static> VfRegistration<'a, F>
where
    for<'b> F::Of<'b>: Send + Sync,
{
    /// Create a new VF registration.
    ///
    /// Returns a pin-initializer so the registration can be embedded directly
    /// in the PF driver's bus device private data. Fails with [`EINVAL`] if
    /// the device is not a PF, or with [`EBUSY`] if a registration already
    /// exists. When `enable` is `true` and `nr_vfs > 0`,
    /// `pci_enable_sriov()` is called after the data is pinned.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the containing struct's field ordering drops
    /// this `VfRegistration` before any resources that the registration data
    /// borrows. The caller must not `mem::forget()` the containing struct.
    pub unsafe fn new<'core, D: PinInit<F::Of<'a>, Error> + 'a>(
        pdev: &'a Device<device::Core<'core>>,
        nr_vfs: u16,
        enable: bool,
        data: D,
    ) -> impl PinInit<Self, Error> + use<'a, 'core, F, D> {
        pin_init::pin_init_scope(move || {
            if !pdev.is_physfn() {
                return Err(EINVAL);
            }
            if !pdev.vf_registration_data_rust().is_null() {
                return Err(EBUSY);
            }

            Ok(try_pin_init!(Self {
                pdev,
                inner <- VfRegistrationData::new(data),
                _pin: PhantomPinned,
                _: {
                    // Store the pointer to the pinned `VfRegistrationData`
                    // on the PCI device so VF drivers can find it.
                    pdev.set_vf_registration_data_rust(
                        core::ptr::from_ref(inner.as_ref().get_ref()).cast_mut().cast(),
                    );
                    if enable && nr_vfs > 0 {
                        // SAFETY: `pdev` is a valid PF device. On failure,
                        // `PinnedDrop` clears the pointer.
                        crate::error::to_result(unsafe {
                            bindings::pci_enable_sriov(pdev.as_raw(), nr_vfs.into())
                        })?;
                    }
                },
            }))
        })
    }
}

// SAFETY: The inner data is `Send + Sync` (enforced by the where clause on
// `new`), and `&Device` is `Send + Sync`.
unsafe impl<F: ForLt> Send for VfRegistration<'_, F> where for<'a> F::Of<'a>: Send {}

// SAFETY: `VfRegistration` doesn't expose mutable access; VF drivers only
// read the data through an immutable pinned reference.
unsafe impl<F: ForLt> Sync for VfRegistration<'_, F> where for<'a> F::Of<'a>: Send {}

#[pinned_drop]
impl<F: ForLt + 'static> PinnedDrop for VfRegistration<'_, F> {
    fn drop(self: Pin<&mut Self>) {
        // SAFETY: `pci_disable_sriov()` is safe to call on any `pci_dev`; it
        // is a no-op if the device has no VFs enabled. When VFs are enabled,
        // this blocks until all VF `remove()` callbacks complete.
        unsafe { bindings::pci_disable_sriov(self.pdev.as_raw()) };

        // After `pci_disable_sriov()` all VFs are gone, so no one can read
        // the pointer anymore.
        self.pdev
            .set_vf_registration_data_rust(core::ptr::null_mut());

        // The pinned `inner` field is dropped automatically after this returns.
    }
}

// SAFETY: `pci::Device` is a transparent wrapper of `struct pci_dev`.
// The offset is guaranteed to point to a valid device field inside `pci::Device`.
unsafe impl<Ctx: device::DeviceContext> device::AsBusDevice<Ctx> for Device<Ctx> {
    const OFFSET: usize = offset_of!(bindings::pci_dev, dev);
}

// SAFETY: `Device` is a transparent wrapper of a type that doesn't depend on `Device`'s generic
// argument.
kernel::impl_device_context_deref!(unsafe { Device });
kernel::impl_device_context_into_aref!(Device);

impl<'a> crate::dma::Device<'a> for Device<device::Core<'a>> {}

// SAFETY: Instances of `Device` are always reference-counted.
unsafe impl crate::sync::aref::AlwaysRefCounted for Device {
    #[inline]
    fn inc_ref(&self) {
        // SAFETY: The existence of a shared reference guarantees that the refcount is non-zero.
        unsafe { bindings::pci_dev_get(self.as_raw()) };
    }

    #[inline]
    unsafe fn dec_ref(obj: NonNull<Self>) {
        // SAFETY: The safety requirements guarantee that the refcount is non-zero.
        unsafe { bindings::pci_dev_put(obj.cast().as_ptr()) }
    }
}

impl<Ctx: device::DeviceContext> AsRef<device::Device<Ctx>> for Device<Ctx> {
    fn as_ref(&self) -> &device::Device<Ctx> {
        // SAFETY: By the type invariant of `Self`, `self.as_raw()` is a pointer to a valid
        // `struct pci_dev`.
        let dev = unsafe { addr_of_mut!((*self.as_raw()).dev) };

        // SAFETY: `dev` points to a valid `struct device`.
        unsafe { device::Device::from_raw(dev) }
    }
}

impl<Ctx: device::DeviceContext> TryFrom<&device::Device<Ctx>> for &Device<Ctx> {
    type Error = kernel::error::Error;

    fn try_from(dev: &device::Device<Ctx>) -> Result<Self, Self::Error> {
        // SAFETY: By the type invariant of `Device`, `dev.as_raw()` is a valid pointer to a
        // `struct device`.
        if !unsafe { bindings::dev_is_pci(dev.as_raw()) } {
            return Err(EINVAL);
        }

        // SAFETY: We've just verified that the bus type of `dev` equals `bindings::pci_bus_type`,
        // hence `dev` must be embedded in a valid `struct pci_dev` as guaranteed by the
        // corresponding C code.
        let pdev = unsafe { container_of!(dev.as_raw(), bindings::pci_dev, dev) };

        // SAFETY: `pdev` is a valid pointer to a `struct pci_dev`.
        Ok(unsafe { &*pdev.cast() })
    }
}

// SAFETY: A `Device` is always reference-counted and can be released from any thread.
unsafe impl Send for Device {}

// SAFETY: `Device` can be shared among threads because all methods of `Device`
// (i.e. `Device<Normal>) are thread safe.
unsafe impl Sync for Device {}

// SAFETY: Same as `Device<Normal>` -- the underlying `struct pci_dev` is the same;
// `Bound` is a zero-sized type-state marker that does not affect thread safety.
unsafe impl Sync for Device<device::Bound> {}
