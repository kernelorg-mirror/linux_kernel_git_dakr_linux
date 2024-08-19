// SPDX-License-Identifier: GPL-2.0

//! Wrappers for the PCI subsystem
//!
//! C header: [`include/linux/pci.h`](srctree/include/linux/pci.h)

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use kernel::{
    alloc::flags::*,
    bindings,
    error::{from_result, to_result},
    prelude::*,
};

/// Drivers must implement this trait to register a PCI driver.
pub trait Driver {
    /// Pointer to the PCI driver's ID table.
    ///
    /// This is highly unsafe, since we have to trust the driver, that the provided pointer is
    /// valid.
    const ID_TABLE: *const bindings::pci_device_id;

    /// PCI driver probe.
    ///
    /// Called when a PCI device is matched against a PCI driver.
    fn probe(pdev: *mut bindings::pci_dev) -> Result;

    /// PCI driver remove.
    ///
    /// Called when the PCI device is unbound.
    fn remove(pdev: *mut bindings::pci_dev);
}

struct Adapter<T: Driver>(PhantomData<T>);

impl<T> Adapter<T>
where
    T: Driver,
{
    extern "C" fn probe(
        pdev: *mut bindings::pci_dev,
        _ent: *const bindings::pci_device_id,
    ) -> core::ffi::c_int {
        from_result(|| {
            T::probe(pdev)?;
            Ok(0)
        })
    }

    extern "C" fn remove(pdev: *mut bindings::pci_dev) {
        T::remove(pdev);
    }
}

/// Registration structure for a PCI driver.
///
/// The existance of an instance of this structure implies that the corresponding PCI driver is
/// currently registered.
pub struct Registration<T: Driver> {
    driver: Pin<KBox<UnsafeCell<bindings::pci_driver>>>,
    _p: PhantomData<T>,
}

impl<T> Registration<T>
where
    T: Driver,
{
    /// Register a new PCI driver from `T: Driver`.
    pub fn new(name: &'static CStr, module: &'static ThisModule) -> Result<Self> {
        let mut driver = KBox::pin(UnsafeCell::new(bindings::pci_driver::default()), GFP_KERNEL)?;

        // Abuse that `bindings::pci_driver` is `Unpin`.
        let inner = driver.get_mut();
        inner.name = name.as_char_ptr();
        inner.probe = Some(Adapter::<T>::probe);
        inner.remove = Some(Adapter::<T>::remove);
        inner.id_table = T::ID_TABLE;

        // SAFETY: `driver` is a valid `struct pci_driver`; `ThisModule` is equivalent to
        // C's `THIS_MODULE` and hence valid for `__pci_register_driver`. `name` is passed as `NULL`
        // terminated C string.
        //
        // Returns zero when the driver was registered successfully, a non-zero error code
        // otherwise, which is handled by `to_result`.
        to_result(unsafe {
            bindings::__pci_register_driver(driver.get(), module.as_ptr(), name.as_char_ptr())
        })?;

        Ok(Self {
            driver,
            _p: PhantomData::<T>,
        })
    }
}

impl<T> Drop for Registration<T>
where
    T: Driver,
{
    fn drop(&mut self) {
        // SAFETY: `Module::drop` is only ever called when `self.drv` was registered
        // successfully.
        unsafe { bindings::pci_unregister_driver(self.driver.get()) };
    }
}

// SAFETY: `Registration` has no fields or methods accessible via `&Registration`, so it is safe to
// share references to it with multiple threads as nothing can be done.
unsafe impl<T> Sync for Registration<T> where T: Driver {}

// SAFETY: Both registration and unregistration are implemented in C and safe to be performed from
// any thread, so `Registration` is `Send`.
unsafe impl<T> Send for Registration<T> where T: Driver {}
