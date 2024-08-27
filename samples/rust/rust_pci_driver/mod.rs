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

#[pin_data]
struct Module {
    #[pin]
    _reg: kernel::driver::Registration<pci::Adapter<driver::Driver>>,
}

impl kernel::InPlaceModule for Module {
    fn init(name: &'static CStr, module: &'static ThisModule) -> impl PinInit<Self, Error> {
        try_pin_init!(Module {
            _reg <- kernel::driver::Registration::new(name, module),
        })
    }
}
