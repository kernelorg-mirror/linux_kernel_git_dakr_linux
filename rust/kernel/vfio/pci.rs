// SPDX-License-Identifier: GPL-2.0

//! VFIO PCI variant driver abstractions.
//!
//! Provides Rust abstractions for writing VFIO PCI variant drivers (also known
//! as "VFIO PCI core" drivers). A [`Registration`] struct owns the VFIO device
//! and its associated registration data, tying resource lifetimes to the PCI
//! driver's binding scope.
//!
//! C header: [`include/linux/vfio_pci_core.h`](srctree/include/linux/vfio_pci_core.h)

use super::{
    InfoCap,
    UserBuf, //
};

use crate::{
    alloc::allocator::Kmalloc,
    bindings,
    device,
    error::{
        from_err_ptr,
        to_result, //
    },
    pci,
    prelude::*,
    sync::aref::{ARef, AlwaysRefCounted},
    types::Opaque,
    uaccess::UserPtr, //
};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    marker::PhantomData,
    ptr::NonNull, //
};

/// Index of the PCI configuration-space region.
pub const CONFIG_REGION_INDEX: u32 = bindings::VFIO_PCI_CONFIG_REGION_INDEX;

/// The context in which a VFIO PCI device reference is valid.
///
/// Callback views can only be borrowed from the corresponding VFIO trampoline.
/// They cannot be refcounted or shared across threads.
pub trait DeviceContext: private::Sealed {}

/// An ordinary device reference, without callback-specific operations.
pub struct Normal;

/// The device is borrowed for a VFIO ioctl callback on the current thread.
pub struct Ioctl;

/// The device is borrowed for a VFIO read callback on the current thread.
pub struct Read;

/// The device is borrowed for a VFIO region-info callback on the current thread.
pub struct GetRegionInfo;

mod private {
    pub trait Sealed {}

    impl Sealed for super::Normal {}
    impl Sealed for super::Ioctl {}
    impl Sealed for super::Read {}
    impl Sealed for super::GetRegionInfo {}
}

impl DeviceContext for Normal {}
impl DeviceContext for Ioctl {}
impl DeviceContext for Read {}
impl DeviceContext for GetRegionInfo {}

/// The trait for VFIO PCI variant driver implementations.
///
/// Only the callbacks that the nvidia-vgpu driver needs are required; the
/// remaining `vfio_device_ops` callbacks are wired to their `vfio_pci_core_*`
/// defaults.
///
/// # Lifetime of `RegistrationData`
///
/// The `RegistrationData<'a>` associated type can borrow from the PCI binding
/// scope (`'a`). It is owned by [`Registration`] and stored alongside the
/// VFIO device. Each callback receives a reference to this data.
pub trait Operations: Sized + Send + Sync + 'static {
    /// The name of the VFIO driver.
    const NAME: &'static CStr;

    /// Data owned by the [`Registration`] and passed to VFIO callbacks.
    ///
    /// Per-device state (e.g. the GFID, cached type info) lives here.
    type RegistrationData<'a>: Send + Sync + 'a;

    /// Data shared by callbacks from the first successful open until the last close.
    /// May borrow registration data; dropped before PCI core close on the last close.
    type OpenData<'a>: Send + Sync + 'a;

    /// Called when the first file descriptor is opened for this device.
    ///
    /// `vfio_pci_core_enable()` has already succeeded; if this returns an
    /// error, `vfio_pci_core_disable()` is called automatically.
    fn open_device<'a>(
        dev: &'a Device<Self>,
        reg_data: &'a Self::RegistrationData<'a>,
    ) -> impl PinInit<Self::OpenData<'a>, Error> + 'a;

    /// Handle a VFIO ioctl.
    ///
    /// The default `vfio_pci_core_ioctl()` is available via
    /// [`Device::core_ioctl()`] for delegation.
    fn ioctl<'a>(
        dev: &Device<Self, Ioctl>,
        reg_data: &Self::RegistrationData<'a>,
        open_data: Pin<&Self::OpenData<'a>>,
        cmd: u32,
        arg: usize,
    ) -> Result<isize>;

    /// Read from the device.
    ///
    /// `buf` is a user-space buffer provided by the VFIO core. Use
    /// [`Device::core_read()`] to delegate the default read, and
    /// [`UserBuf::write_at()`] to override specific bytes afterwards.
    fn read<'a>(
        dev: &Device<Self, Read>,
        reg_data: &Self::RegistrationData<'a>,
        open_data: Pin<&Self::OpenData<'a>>,
        buf: &mut UserBuf,
        ppos: &mut Position<'_>,
    ) -> Result<isize>;

    /// Fill in region info capabilities.
    ///
    /// The default `vfio_pci_ioctl_get_region_info()` is available via
    /// [`Device::core_get_region_info()`] for delegation.
    fn get_region_info<'a>(
        dev: &Device<Self, GetRegionInfo>,
        reg_data: &Self::RegistrationData<'a>,
        open_data: Pin<&Self::OpenData<'a>>,
        info: &mut bindings::vfio_region_info,
        caps: &mut InfoCap<'_>,
    ) -> Result;
}

/// A VFIO PCI file position, encoding a region index and an offset within that region.
pub struct Position<'a> {
    raw: &'a mut i64,
}

impl Position<'_> {
    /// Returns the PCI region index.
    pub fn region_index(&self) -> u32 {
        ((*self.raw as u64) >> bindings::VFIO_PCI_OFFSET_SHIFT) as u32
    }

    /// Returns the byte offset within the current region.
    pub fn region_offset(&self) -> u64 {
        (*self.raw as u64) & ((1u64 << bindings::VFIO_PCI_OFFSET_SHIFT) - 1)
    }
}

/// A VFIO PCI variant device, wrapping `struct vfio_pci_core_device`.
///
/// The layout is `#[repr(C)]` with `core_device` first, so the
/// `vfio_device` embedded at offset 0 of `vfio_pci_core_device` is also at
/// offset 0 of `Self`, matching the `vfio_alloc_device` requirement.
/// Callback contexts expose only their matching core helper and cannot be
/// refcounted or shared across threads.
///
/// # Invariants
///
/// - Callback contexts are only borrowed for the corresponding callback and thread.
/// - `core_device` was initialized by `vfio_pci_core_init_dev()`.
/// - The VFIO device refcount owns the allocation lifetime.
/// - `reg_data` is dangling outside registration or points to a valid
///   `T::RegistrationData<'_>` owned by the enclosing [`Registration`].
/// - `open_data` is null outside a successful open, otherwise it owns a pinned
///   `KBox<T::OpenData<'_>>` until last close. Only open/close mutate it; VFIO
///   prevents I/O callbacks from racing those transitions. The data is dropped
///   before PCI core close and before registration data is freed.
#[repr(C)]
pub struct Device<T: Operations, Ctx: DeviceContext = Normal> {
    core_device: Opaque<bindings::vfio_pci_core_device>,
    reg_data: UnsafeCell<NonNull<T::RegistrationData<'static>>>,
    open_data: UnsafeCell<*mut T::OpenData<'static>>,
    _context: PhantomData<Ctx>,
}

impl<T: Operations> Device<T> {
    /// Allocate a VFIO PCI device for subsequent registration.
    pub fn new(pdev: &pci::Device<device::Core<'_>>) -> Result<ARef<Self>> {
        const_assert!(core::mem::offset_of!(Self, core_device) == 0);
        let size = Kmalloc::aligned_layout(Layout::new::<Self>()).size();

        // SAFETY: The bound PCI device and static ops are valid. The allocation
        // includes the full Rust wrapper with kmalloc-compatible size and alignment.
        let raw = from_err_ptr(unsafe {
            bindings::_vfio_alloc_device(size, pdev.as_ref().as_raw(), &Self::OPS)
        })?;
        let this = NonNull::new(raw.cast::<Self>()).ok_or(ENOMEM)?;

        // SAFETY: Initialise the Rust field in the newly allocated device.
        unsafe {
            (&raw mut (*this.as_ptr()).reg_data).write(UnsafeCell::new(NonNull::dangling()));
            (&raw mut (*this.as_ptr()).open_data).write(UnsafeCell::new(core::ptr::null_mut()));
            (&raw mut (*this.as_ptr())._context).write(PhantomData);
        }

        // SAFETY: `this` owns the initial VFIO device reference.
        Ok(unsafe { ARef::from_raw(this) })
    }

    /// Access the registration data through a closure with an HRTB lifetime.
    ///
    /// The closure's `for<'a>` bound prevents the caller from smuggling
    /// references with a concrete short lifetime out of the closure.
    fn registration_data_with<R>(
        &self,
        f: impl for<'a> FnOnce(&'a T::RegistrationData<'a>) -> R,
    ) -> R {
        // SAFETY: The registration data pointer is set by `Registration::new()`
        // before the device is registered, and cleared only after unregistration
        // completes. The HRTB prevents lifetime smuggling. The cast from
        // `'static` to `'_` is layout-compatible.
        let reg_data: &T::RegistrationData<'_> = unsafe {
            (*self.reg_data.get())
                .cast::<T::RegistrationData<'_>>()
                .as_ref()
        };
        f(reg_data)
    }

    /// # Safety
    /// Must be called from an I/O callback while VFIO keeps the device open.
    unsafe fn open_data<'a>(&self) -> Pin<&T::OpenData<'a>> {
        // SAFETY: VFIO excludes open/close transitions while this callback runs.
        let ptr = unsafe { *self.open_data.get() }.cast::<T::OpenData<'a>>();
        // SAFETY: The allocation stays pinned until last close. The erased lifetime
        // is restored only for an HRTB callback, alongside the registration data.
        unsafe { Pin::new_unchecked(&*ptr) }
    }

    /// # Safety
    /// The returned view must only be used during the callback represented by `Ctx`.
    unsafe fn with_context<Ctx: DeviceContext>(&self) -> &Device<T, Ctx> {
        // SAFETY: All contexts have the same layout; the caller guarantees the context.
        unsafe { &*core::ptr::from_ref(self).cast() }
    }

    /// Recover `&Self` from a raw `*mut vfio_device` in a callback.
    ///
    /// # Safety
    ///
    /// `vdev` must point to the `vfio_device` at the start of a
    /// `Device<T>` allocated by `_vfio_alloc_device()` and remain valid for `'a`.
    unsafe fn from_vfio_device<'a>(vdev: *mut bindings::vfio_device) -> &'a Self {
        // SAFETY: `vfio_device` is at offset 0 of `vfio_pci_core_device`,
        // which is at offset 0 of `Device<T>`, so the pointer cast is
        // valid.
        unsafe { &*(vdev.cast()) }
    }
}

impl<T: Operations, Ctx: DeviceContext> Device<T, Ctx> {
    fn core_device(&self) -> *mut bindings::vfio_pci_core_device {
        self.core_device.get()
    }

    fn vfio_device(&self) -> *mut bindings::vfio_device {
        self.core_device.get().cast()
    }
}

impl<T: Operations> Device<T, Ioctl> {
    /// Delegate to `vfio_pci_core_ioctl()`.
    pub fn core_ioctl(&self, cmd: u32, arg: usize) -> Result<isize> {
        // SAFETY: The Ioctl context keeps this device open on the callback thread.
        let ret = unsafe { bindings::vfio_pci_core_ioctl(self.vfio_device(), cmd, arg) };
        if ret < 0 {
            Err(Error::from_errno(ret as i32))
        } else {
            Ok(ret)
        }
    }
}

impl<T: Operations> Device<T, Read> {
    /// Delegate to `vfio_pci_core_read()`.
    ///
    /// The VFIO core fills the user-space buffer with the default PCI data
    /// for the region identified by `ppos` and advances the position by the bytes read.
    pub fn core_read(&self, buf: &UserBuf, ppos: &mut Position<'_>) -> Result<isize> {
        // SAFETY: The Read context keeps this device open on the callback thread;
        // `buf.ptr` is a user-space pointer supplied by VFIO.
        let ret = unsafe {
            bindings::vfio_pci_core_read(
                self.vfio_device(),
                buf.ptr.as_mut_ptr().cast(),
                buf.count,
                ppos.raw,
            )
        };
        if ret < 0 {
            Err(Error::from_errno(ret as i32))
        } else {
            Ok(ret)
        }
    }
}

impl<T: Operations> Device<T, GetRegionInfo> {
    /// Delegate to `vfio_pci_ioctl_get_region_info()`.
    pub fn core_get_region_info(
        &self,
        info: &mut bindings::vfio_region_info,
        caps: &mut InfoCap<'_>,
    ) -> Result {
        // SAFETY: GetRegionInfo keeps the device open on the callback thread;
        // `InfoCap` preserves the validity of VFIO's capability buffer.
        to_result(unsafe {
            bindings::vfio_pci_ioctl_get_region_info(self.vfio_device(), info, caps.raw)
        })
    }
}

// SAFETY: The embedded device reference count owns the VFIO allocation.
unsafe impl<T: Operations> AlwaysRefCounted for Device<T> {
    fn inc_ref(&self) {
        // SAFETY: `self` holds a live VFIO device reference.
        unsafe { bindings::get_device(&raw mut (*self.vfio_device()).device) };
    }

    unsafe fn dec_ref(obj: NonNull<Self>) {
        // SAFETY: The caller owns the reference being released.
        unsafe { bindings::put_device(&raw mut (*obj.as_ref().vfio_device()).device) };
    }
}

// SAFETY: VFIO serializes open/close transitions, and callback data is Send + Sync.
unsafe impl<T: Operations> Send for Device<T> {}

// SAFETY: I/O callbacks share Sync data. VFIO excludes them from mutations of
// `open_data` during first open and last close; registration data follows the
// registration lifetime. Mutable C state is managed by the VFIO core.
unsafe impl<T: Operations> Sync for Device<T> {}

// --- C callback trampolines ------------------------------------------------
//
// These are private `extern "C"` functions called by the VFIO core through the
// vtable. The VFIO core guarantees that `vdev` is valid.
#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn open_device_cb<T: Operations>(
    vdev: *mut bindings::vfio_device,
) -> core::ffi::c_int {
    // SAFETY: `vdev` is valid; set by vfio_alloc_device.
    let dev = unsafe { Device::<T>::from_vfio_device(vdev) };

    // SAFETY: Enable the PCI-core side first.
    let ret = unsafe { bindings::vfio_pci_core_enable(dev.core_device()) };
    if ret != 0 {
        return ret;
    }

    let result = dev.registration_data_with(|rd| {
        // Allocate before calling the driver so allocation failure cannot follow open.
        let data = KBox::<T::OpenData<'_>>::new_uninit(GFP_KERNEL)?;
        let data = data.write_pin_init(T::open_device(dev, rd))?;

        // SAFETY: Only the owning pointer is moved; the allocation remains pinned.
        let raw = KBox::into_raw(unsafe { Pin::into_inner_unchecked(data) })
            .cast::<T::OpenData<'static>>();
        // SAFETY: First open is exclusive with I/O callbacks and last close.
        // Lifetimes do not affect layout; the device owns the allocation until close.
        unsafe { *dev.open_data.get() = raw };
        Ok::<(), Error>(())
    });
    match result {
        Ok(()) => {
            // SAFETY: open succeeded.
            unsafe { bindings::vfio_pci_core_finish_enable(dev.core_device()) };
            0
        }
        Err(e) => {
            // SAFETY: Undo the enable on failure.
            unsafe { bindings::vfio_pci_core_disable(dev.core_device()) };
            e.to_errno()
        }
    }
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn close_device_cb<T: Operations>(vdev: *mut bindings::vfio_device) {
    // SAFETY: `vdev` is valid.
    let dev = unsafe { Device::<T>::from_vfio_device(vdev) };
    // SAFETY: Last close is exclusive with I/O callbacks and the next first open.
    let raw = unsafe { core::mem::replace(&mut *dev.open_data.get(), core::ptr::null_mut()) };
    // Run the driver's destructor while registration data and PCI resources are valid.
    // SAFETY: Successful open transferred this allocation with `KBox::into_raw`.
    // Last close takes ownership exactly once and drops it without moving the pointee.
    unsafe { drop(KBox::from_raw(raw)) };
    // SAFETY: Matches the enable in open_device_cb.
    unsafe { bindings::vfio_pci_core_close_device(vdev) };
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn ioctl_cb<T: Operations>(
    vdev: *mut bindings::vfio_device,
    cmd: core::ffi::c_uint,
    arg: usize,
) -> isize {
    // SAFETY: `vdev` is valid.
    let dev = unsafe { Device::<T>::from_vfio_device(vdev) };
    // SAFETY: VFIO invokes this callback for an open device with valid arguments.
    match dev.registration_data_with(|rd| unsafe {
        T::ioctl(dev.with_context::<Ioctl>(), rd, dev.open_data(), cmd, arg)
    }) {
        Ok(v) => v,
        Err(e) => e.to_errno() as isize,
    }
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn read_cb<T: Operations>(
    vdev: *mut bindings::vfio_device,
    buf: *mut u8,
    count: usize,
    ppos: *mut i64,
) -> isize {
    // SAFETY: `vdev` is valid. `buf` is a valid user-space pointer provided by
    // the VFIO core. `ppos` is a valid kernel pointer.
    let dev = unsafe { Device::<T>::from_vfio_device(vdev) };
    let mut ubuf = UserBuf {
        ptr: UserPtr::from_ptr(buf.cast()),
        count,
    };
    let mut pos = Position {
        // SAFETY: `ppos` is a valid kernel pointer provided by the VFIO core.
        raw: unsafe { &mut *ppos },
    };
    // SAFETY: VFIO invokes this callback for an open device with valid arguments.
    match dev.registration_data_with(|rd| unsafe {
        T::read(
            dev.with_context::<Read>(),
            rd,
            dev.open_data(),
            &mut ubuf,
            &mut pos,
        )
    }) {
        Ok(n) => n,
        Err(e) => e.to_errno() as isize,
    }
}

#[allow(clippy::missing_safety_doc)]
unsafe extern "C" fn get_region_info_cb<T: Operations>(
    vdev: *mut bindings::vfio_device,
    info: *mut bindings::vfio_region_info,
    caps: *mut bindings::vfio_info_cap,
) -> core::ffi::c_int {
    // SAFETY: `vdev`, `info`, and `caps` are valid kernel pointers provided
    // by the VFIO core.
    let dev = unsafe { Device::<T>::from_vfio_device(vdev) };
    // SAFETY: `info` is a valid pointer provided by the VFIO core.
    let info = unsafe { &mut *info };
    let mut caps = InfoCap {
        // SAFETY: `caps` is a valid pointer provided by the VFIO core.
        raw: unsafe { &mut *caps },
    };
    // SAFETY: VFIO invokes this callback for an open device with valid arguments.
    match dev.registration_data_with(|rd| unsafe {
        T::get_region_info(
            dev.with_context::<GetRegionInfo>(),
            rd,
            dev.open_data(),
            info,
            &mut caps,
        )
    }) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

/// Build the `vfio_device_ops` vtable for a variant driver `T`.
///
/// Custom callbacks (`open_device`, `close_device`, `ioctl`, `read`,
/// `get_region_info_caps`) dispatch to `T`'s trait implementation.
/// All other callbacks use the `vfio_pci_core_*` defaults.
macro_rules! build_ops {
    ($T:ty) => {
        bindings::vfio_device_ops {
            #[allow(clippy::disallowed_methods)]
            name: <$T>::NAME.as_ptr().cast_mut().cast(),
            init: Some(bindings::vfio_pci_core_init_dev),
            release: Some(bindings::vfio_pci_core_release_dev),
            open_device: Some(open_device_cb::<$T>),
            close_device: Some(close_device_cb::<$T>),
            ioctl: Some(ioctl_cb::<$T>),
            read: Some(read_cb::<$T>),
            write: Some(bindings::vfio_pci_core_write),
            mmap: Some(bindings::vfio_pci_core_mmap),
            request: Some(bindings::vfio_pci_core_request),
            get_region_info_caps: Some(get_region_info_cb::<$T>),
            match_: Some(bindings::vfio_pci_core_match),
            match_token_uuid: Some(bindings::vfio_pci_core_match_token_uuid),
            device_feature: Some(bindings::vfio_pci_core_ioctl_feature),
            #[cfg(CONFIG_IOMMUFD)]
            bind_iommufd: Some(bindings::vfio_iommufd_physical_bind),
            #[cfg(not(CONFIG_IOMMUFD))]
            bind_iommufd: None,
            #[cfg(CONFIG_IOMMUFD)]
            unbind_iommufd: Some(bindings::vfio_iommufd_physical_unbind),
            #[cfg(not(CONFIG_IOMMUFD))]
            unbind_iommufd: None,
            #[cfg(CONFIG_IOMMUFD)]
            attach_ioas: Some(bindings::vfio_iommufd_physical_attach_ioas),
            #[cfg(not(CONFIG_IOMMUFD))]
            attach_ioas: None,
            #[cfg(CONFIG_IOMMUFD)]
            detach_ioas: Some(bindings::vfio_iommufd_physical_detach_ioas),
            #[cfg(not(CONFIG_IOMMUFD))]
            detach_ioas: None,
            pasid_attach_ioas: None,
            pasid_detach_ioas: None,
            dma_unmap: None,
        }
    };
}

/// The registration of a VFIO PCI variant device.
///
/// Owns both the VFIO device reference and the registration data, tying
/// resource lifetimes to the PCI binding scope `'a`.
///
/// When dropped, the device is unregistered and its registration data is freed.
pub struct Registration<'a, T: Operations> {
    dev: ARef<Device<T>>,
    _reg_data: Pin<KBox<T::RegistrationData<'a>>>,
}

impl<T: Operations> Device<T> {
    const OPS: bindings::vfio_device_ops = build_ops!(T);
}

impl<'a, T: Operations> Registration<'a, T> {
    /// Register a previously allocated VFIO PCI device with callback data.
    ///
    /// # Safety
    ///
    /// The returned registration must be dropped before the PCI driver unbinds.
    /// `dev` must have been allocated during this binding of `pdev`, must never
    /// have been registered, and must not be registered concurrently elsewhere.
    pub unsafe fn new(
        pdev: &'a pci::Device<device::Core<'_>>,
        dev: &Device<T>,
        reg_data: impl PinInit<T::RegistrationData<'a>, Error>,
    ) -> Result<Self> {
        // SAFETY: `dev` holds a live VFIO allocation; only compare its parent pointer.
        if unsafe { (*dev.vfio_device()).dev } != pdev.as_ref().as_raw() {
            return Err(EINVAL);
        }

        let reg_data: Pin<KBox<T::RegistrationData<'a>>> = KBox::pin_init(reg_data, GFP_KERNEL)?;
        let ptr: NonNull<T::RegistrationData<'static>> =
            NonNull::from(Pin::get_ref(reg_data.as_ref())).cast();

        // SAFETY: No concurrent registration or callbacks; publish before registering.
        unsafe { *dev.reg_data.get() = ptr };

        // SAFETY: The device is initialised, and its PCI parent is still bound.
        let ret = unsafe { bindings::vfio_pci_core_register_device(dev.core_device()) };
        if ret != 0 {
            // SAFETY: Registration failed and no callbacks can access the data.
            unsafe { *dev.reg_data.get() = NonNull::dangling() };
            return Err(Error::from_errno(ret));
        }

        Ok(Self {
            dev: dev.into(),
            _reg_data: reg_data,
        })
    }

    /// Returns a reference to the [`Device`].
    pub fn device(&self) -> &Device<T> {
        &self.dev
    }
}

impl<T: Operations> Drop for Registration<'_, T> {
    fn drop(&mut self) {
        // SAFETY: The parent is still bound. Unregistration waits for all open
        // file descriptors and callbacks to finish.
        unsafe { bindings::vfio_pci_core_unregister_device(self.dev.core_device()) };

        // SAFETY: No callbacks can access the data after unregistration.
        unsafe { *self.dev.reg_data.get() = NonNull::dangling() };

        // The ARef and callback data are released after unregistration.
    }
}
