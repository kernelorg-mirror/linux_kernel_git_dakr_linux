// SPDX-License-Identifier: GPL-2.0 OR MIT

//! DRM driver core.
//!
//! C header: [`include/drm/drm_drv.h`](srctree/include/drm/drm_drv.h)

use crate::{
    bindings,
    device,
    devres,
    drm,
    error::to_result,
    prelude::*,
    sync::aref::ARef,
    types::ForLt, //
};
use core::{
    mem,
    ptr::NonNull, //
};

/// Driver use the GEM memory manager. This should be set for all modern drivers.
pub(crate) const FEAT_GEM: u32 = bindings::drm_driver_feature_DRIVER_GEM;

/// Information data for a DRM Driver.
pub struct DriverInfo {
    /// Driver major version.
    pub major: i32,
    /// Driver minor version.
    pub minor: i32,
    /// Driver patchlevel version.
    pub patchlevel: i32,
    /// Driver name.
    pub name: &'static CStr,
    /// Driver description.
    pub desc: &'static CStr,
}

/// Internal memory management operation set, normally created by memory managers (e.g. GEM).
pub struct AllocOps {
    pub(crate) gem_create_object: Option<
        unsafe extern "C" fn(
            dev: *mut bindings::drm_device,
            size: usize,
        ) -> *mut bindings::drm_gem_object,
    >,
    pub(crate) prime_handle_to_fd: Option<
        unsafe extern "C" fn(
            dev: *mut bindings::drm_device,
            file_priv: *mut bindings::drm_file,
            handle: u32,
            flags: u32,
            prime_fd: *mut core::ffi::c_int,
        ) -> core::ffi::c_int,
    >,
    pub(crate) prime_fd_to_handle: Option<
        unsafe extern "C" fn(
            dev: *mut bindings::drm_device,
            file_priv: *mut bindings::drm_file,
            prime_fd: core::ffi::c_int,
            handle: *mut u32,
        ) -> core::ffi::c_int,
    >,
    pub(crate) gem_prime_import: Option<
        unsafe extern "C" fn(
            dev: *mut bindings::drm_device,
            dma_buf: *mut bindings::dma_buf,
        ) -> *mut bindings::drm_gem_object,
    >,
    pub(crate) gem_prime_import_sg_table: Option<
        unsafe extern "C" fn(
            dev: *mut bindings::drm_device,
            attach: *mut bindings::dma_buf_attachment,
            sgt: *mut bindings::sg_table,
        ) -> *mut bindings::drm_gem_object,
    >,
    pub(crate) dumb_create: Option<
        unsafe extern "C" fn(
            file_priv: *mut bindings::drm_file,
            dev: *mut bindings::drm_device,
            args: *mut bindings::drm_mode_create_dumb,
        ) -> core::ffi::c_int,
    >,
    pub(crate) dumb_map_offset: Option<
        unsafe extern "C" fn(
            file_priv: *mut bindings::drm_file,
            dev: *mut bindings::drm_device,
            handle: u32,
            offset: *mut u64,
        ) -> core::ffi::c_int,
    >,
}

/// Trait for memory manager implementations. Implemented internally.
pub trait AllocImpl: super::private::Sealed + drm::gem::IntoGEMObject {
    /// The [`Driver`] implementation for this [`AllocImpl`].
    type Driver: drm::Driver;

    /// The C callback operations for this memory manager.
    const ALLOC_OPS: AllocOps;
}

/// The DRM `Driver` trait.
///
/// This trait must be implemented by drivers in order to create a `struct drm_device` and `struct
/// drm_driver` to be registered in the DRM subsystem.
#[vtable]
pub trait Driver {
    /// Context data associated with the DRM driver
    type Data: Sync + Send;

    /// Data owned by the [`Registration`] and accessible through [`drm::device::UnbindGuard`].
    ///
    /// This is a [`ForLt`](trait@ForLt) type whose lifetime is tied to the parent bus
    /// device binding scope.
    /// The data is only accessible while the parent bus device is bound (i.e. within a
    /// `drm_dev_enter/exit` critical section), and references handed out by
    /// [`UnbindGuard::registration_data()`](drm::device::UnbindGuard::registration_data) have
    /// their lifetime shortened accordingly via [`ForLt::cast_ref`].
    type RegistrationData: ForLt;

    /// The type used to manage memory for this driver.
    type Object<Ctx: drm::DeviceContext>: AllocImpl;

    /// The type used to represent a DRM File (client)
    type File: drm::file::DriverFile;

    /// The bus device type of the parent device that the DRM device is associated with.
    type ParentDevice<Ctx: device::DeviceContext>: device::AsBusDevice<Ctx>;

    /// Driver metadata
    const INFO: DriverInfo;

    /// IOCTL list. See `kernel::drm::ioctl::declare_drm_ioctls!{}`.
    const IOCTLS: &'static [drm::ioctl::DrmIoctlDescriptor];
}

/// The registration type of a `drm::Device`.
///
/// Once the `Registration` structure is dropped, the device is unregistered.
pub struct Registration<T: Driver> {
    drm: ARef<drm::Device<T>>,
    #[allow(dead_code)]
    reg_data: Pin<KBox<<T::RegistrationData as ForLt>::Of<'static>>>,
}

impl<T: Driver> Registration<T>
where
    for<'a> <T::RegistrationData as ForLt>::Of<'a>: Send,
{
    fn new<'bound, E>(
        drm: drm::UnregisteredDevice<T>,
        reg_data: impl PinInit<<T::RegistrationData as ForLt>::Of<'bound>, E>,
        flags: usize,
    ) -> Result<Self>
    where
        Error: From<E>,
    {
        let reg_data: Pin<KBox<<T::RegistrationData as ForLt>::Of<'bound>>> =
            KBox::pin_init(reg_data, GFP_KERNEL)?;

        // SAFETY: `ForLt` guarantees covariance; lifetimes do not affect layout.
        let reg_data: Pin<KBox<<T::RegistrationData as ForLt>::Of<'static>>> =
            unsafe { mem::transmute(reg_data) };

        // Store the registration data pointer in the device before registration, so that it is
        // visible once ioctls can be called.
        //
        // SAFETY: No concurrent access; the device is not yet registered.
        unsafe { *drm.registration_data.get() = NonNull::from(Pin::get_ref(reg_data.as_ref())) }

        // SAFETY: `drm` is a valid, initialized but not yet registered DRM device.
        let ret = unsafe { bindings::drm_dev_register(drm.as_raw(), flags) };
        if let Err(e) = to_result(ret) {
            // SAFETY: No concurrent access; registration failed.
            unsafe { *drm.registration_data.get() = NonNull::dangling() };
            return Err(e);
        }

        // SAFETY: We just called `drm_dev_register` above
        let new = NonNull::from(unsafe { drm.assume_ctx() });

        // Leak the ARef from UnregisteredDevice in preparation for transferring its ownership.
        mem::forget(drm);

        // SAFETY: `drm`'s `Drop` constructor was never called, ensuring that there remains at least
        // one reference to the device - which we take ownership over here.
        let new = unsafe { ARef::from_raw(new) };

        Ok(Self { drm: new, reg_data })
    }

    /// Registers a new [`UnregisteredDevice`](drm::UnregisteredDevice) with userspace.
    ///
    /// Ownership of the [`Registration`] object is passed to [`devres::register`].
    pub fn new_foreign_owned<'bound, E>(
        drm: drm::UnregisteredDevice<T>,
        dev: &'bound device::Device<device::Bound>,
        reg_data: impl PinInit<<T::RegistrationData as ForLt>::Of<'bound>, E>,
        flags: usize,
    ) -> Result<&'bound drm::Device<T>>
    where
        T: 'static,
        Error: From<E>,
    {
        if drm.as_ref().as_raw() != dev.as_raw() {
            return Err(EINVAL);
        }

        let reg = Registration::<T>::new(drm, reg_data, flags)?;
        let drm = NonNull::from(reg.device());

        devres::register::<_, core::convert::Infallible>(dev, reg, GFP_KERNEL)?;

        // SAFETY: Since `reg` was passed to devres::register(), the device now owns the lifetime
        // of the DRM registration - ensuring that this reference lives for
        // at least as long as 'bound.
        Ok(unsafe { drm.as_ref() })
    }

    /// Returns a reference to the `Device` instance for this registration.
    pub fn device(&self) -> &drm::Device<T> {
        &self.drm
    }
}

// SAFETY: `Registration` doesn't offer any methods or access to fields when shared between
// threads, hence it's safe to share it.
unsafe impl<T: Driver> Sync for Registration<T> where
    for<'a> <T::RegistrationData as ForLt>::Of<'a>: Send
{
}

// SAFETY: Registration with and unregistration from the DRM subsystem can happen from any thread.
unsafe impl<T: Driver> Send for Registration<T> where
    for<'a> <T::RegistrationData as ForLt>::Of<'a>: Send
{
}

impl<T: Driver> Drop for Registration<T> {
    fn drop(&mut self) {
        // Use `drm_dev_unplug` rather than `drm_dev_unregister` to ensure that existing
        // `drm_dev_enter()` critical sections complete before unregistration proceeds. This
        // is required for the safety of `UnbindGuard`, which relies on the SRCU barrier in
        // `drm_dev_unplug()` to guarantee that the parent device is still bound within the
        // critical section.
        //
        // SAFETY: Safe by the invariant of `ARef<drm::Device<T>>`. The existence of this
        // `Registration` also guarantees that this `drm::Device` is actually registered.
        unsafe { bindings::drm_dev_unplug(self.drm.as_raw()) };
        // After drm_dev_unplug(), the SRCU barrier guarantees that all UnbindGuard critical
        // sections have completed, so no one holds a reference to reg_data anymore.
        // reg_data is dropped here automatically.
    }
}
