// SPDX-License-Identifier: GPL-2.0

//! Generic driver support

use crate::types::Opaque;
use kernel::prelude::*;

/// The [`RegistrationOps`] trait serves as generic interface for subsystems (e.g., PCI, Platform,
/// Amba, etc.) to privide the corresponding subsystem specific implementation to register /
/// unregister a driver of the particular type (`RegType`).
///
/// For instance, the PCI subsystem would set `RegType` to `bindings::pci_driver` and call
/// `bindings::__pci_register_driver` from `RegistrationOps::register` and
/// `bindings::pci_unregister_driver` from `RegistrationOps::unregister`.
pub trait RegistrationOps {
    /// The type that holds information about the registration. This is typically a struct defined
    /// by the C portion of the kernel, e.g. `bindings::pci_driver.
    type RegType: Default;

    /// Registers a driver.
    ///
    /// # Safety
    ///
    /// `reg` must point to valid, initialised, and writable memory. It may be modified by this
    /// function to hold registration state.
    ///
    /// On success, `reg` must remain pinned and valid until the matching call to
    /// [`RegistrationOps::unregister`].
    unsafe fn register(
        reg: *mut Self::RegType,
        name: &'static CStr,
        module: &'static ThisModule,
    ) -> Result;

    /// Unregisters a driver previously registered with [`RegistrationOps::register`].
    ///
    /// # Safety
    ///
    /// `reg` must point to valid writable memory, initialised by a previous successful call to
    /// [`RegistrationOps::register`].
    unsafe fn unregister(reg: *mut Self::RegType);
}

/// Registration structure for a driver.
///
/// The existance of an instance of this structure implies that the corresponding driver is
/// currently registered.
#[pin_data(PinnedDrop)]
pub struct Registration<T: RegistrationOps> {
    #[pin]
    driver: Opaque<T::RegType>,
}

impl<T> Registration<T>
where
    T: RegistrationOps,
{
    /// Register a new driver from `T::RegType`.
    pub fn new(name: &'static CStr, module: &'static ThisModule) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            driver <- Opaque::try_ffi_init(|ptr: *mut T::RegType| {
                // SAFETY: `try_ffi_init` guarantees that `ptr` is valid for write.
                unsafe { ptr.write(T::RegType::default()) };

                // SAFETY: `driver` has just been initialized; `T::unregister` is called on
                // `Self::drop`.
                unsafe { T::register(ptr, name, module) }
            }),
        })
    }
}

#[pinned_drop]
impl<T> PinnedDrop for Registration<T>
where
    T: RegistrationOps,
{
    fn drop(self: Pin<&mut Self>) {
        // SAFETY: Only ever called if the `Registration` was created successfully.
        unsafe { T::unregister(self.driver.get()) };
    }
}

// SAFETY: `Registration` has no fields or methods accessible via `&Registration`, so it is safe to
// share references to it with multiple threads as nothing can be done.
unsafe impl<T> Sync for Registration<T> where T: RegistrationOps {}

// SAFETY: Both registration and unregistration are implemented in C and safe to be performed from
// any thread, so `Registration` is `Send`.
unsafe impl<T> Send for Registration<T> where T: RegistrationOps {}
