// SPDX-License-Identifier: GPL-2.0

//! Rust PCI driver sample.

mod driver;

use core::ptr;
use kernel::{bindings, error::to_result, prelude::*};

module! {
    type: Module,
    name: "rust_pci_driver_sample",
    author: "Danilo Krummrich",
    description: "Rust PCI driver sample",
    license: "GPL",
}

struct Module;

impl kernel::Module for Module {
    fn init(name: &'static CStr, module: &'static ThisModule) -> Result<Self> {
        // SAFETY: `driver::DRIVER` is a valid `struct pci_driver`; `ThisModule` is equivalent to
        // C's `THIS_MODULE` and hence valid for `__pci_register_driver`. `name` is passed as `NULL`
        // terminated C string.
        //
        // Returns zero when the driver was registered successfully, a non-zero error code
        // otherwise, which is handled by `to_result`.
        to_result(unsafe {
            bindings::__pci_register_driver(
                ptr::addr_of_mut!(driver::DRIVER),
                module.as_ptr(),
                name.as_char_ptr(),
            )
        })?;

        Ok(Module)
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: `Module::drop` is only ever called when `driver::DRIVER` was registered
        // successfully.
        unsafe { bindings::pci_unregister_driver(ptr::addr_of_mut!(driver::DRIVER)) };
    }
}
