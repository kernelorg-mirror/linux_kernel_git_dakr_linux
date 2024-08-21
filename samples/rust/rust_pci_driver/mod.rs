// SPDX-License-Identifier: GPL-2.0

//! Rust PCI driver sample.

mod driver;

use kernel::{pci, prelude::*};

module! {
    type: Module,
    name: "rust_pci_driver_sample",
    author: "Danilo Krummrich",
    description: "Rust PCI driver sample",
    license: "GPL",
}

struct Module {
    _reg: kernel::driver::Registration<pci::Adapter<driver::Driver>>,
}

impl kernel::Module for Module {
    fn init(name: &'static CStr, module: &'static ThisModule) -> Result<Self> {
        Ok(Module {
            _reg: kernel::driver::Registration::new(name, module)?,
        })
    }
}
