// SPDX-License-Identifier: GPL-2.0

//! Rust EDU driver sample (based on QEMU's `edu`).
//!
//! To make this driver probe, QEMU must be run with `-device edu`.

use kernel::{
    device::Bound,
    dma::{
        Coherent,
        Device,
        DmaMask, //
    },
    io::{
        io_read,
        io_write,
        poll::read_poll_timeout,
        register,
        register::Array,
        Io, //
    },
    irq::{
        self,
        Flags, //
    },
    pci::{
        self,
        IrqTypes, //
    },
    prelude::*,
    sync::Completion,
    time::Delta, //
};

const QEMU_VENDOR_ID: u16 = 0x1234;
const QEMU_EDU_DEVICE_ID: u32 = 0x11e8;
const QEMU_EDU_DEVICE_MAGIC: u8 = 0xed;
const QEMU_DMA_BASE: u64 = 0x40000;

const IRQ_MAGIC_VALUE: u32 = 42;

/// Bit set in `IRQ_STATUS` when a DMA transfer has completed.
const DMA_IRQ: u32 = 0x100;

mod regs {
    use super::*;

    register! {
        pub(super) IDENTIFICATION(u32) @ 0x0 {
            31:24 major;
            23:16 minor;
            7:0 magic;
        }

        pub(super) LIVENESS_CHECK(u32) @ 0x04 {}

        pub(super) FACTORIAL(u32) @ 0x08 {}

        pub(super) STATUS(u32) @ 0x20 {
            0:0 computing;
            7:7 raise_interrupt;
        }

        pub(super) IRQ_STATUS(u32) @ 0x24 {}
        pub(super) IRQ_RAISE(u32) @ 0x60 {}
        pub(super) IRQ_ACK(u32) @ 0x64 {}

        pub(super) DMA_SRC(u64) @ 0x80 {}
        pub(super) DMA_DST(u64) @ 0x88 {}
        pub(super) DMA_COUNT(u64) @ 0x90 {}
        pub(super) DMA_COMMAND(u64) @ 0x98 {
            0:0 start_transfer;
            1:1 direction;
            2:2 raise_irq;
        }
    }

    pub(super) const END: usize = 0xA0;
}

kernel::pci_device_table!(
    PCI_TABLE,
    <EduDriver as pci::Driver>::IdInfo,
    [(
        pci::DeviceId::from_id(pci::Vendor::from_raw(QEMU_VENDOR_ID), QEMU_EDU_DEVICE_ID),
        ()
    )]
);

type Bar0<'a> = pci::Bar<'a, { regs::END }>;

struct EduDriver;

#[pin_data(PinnedDrop)]
struct EduDriverData<'bound> {
    pdev: &'bound pci::Device<Bound>,
    _enable: pci::DeviceEnableGuard<'bound>,
    dma: Coherent<'bound, u64>,
    #[pin]
    irq_handler: irq::Registration<'_irq, IrqHandler<'_irq>>,
    _irq: pci::IrqVectorRegistration<'bar>,
    bar: Bar0<'bound>,
}

#[pin_data]
struct IrqHandler<'a> {
    pdev: &'a pci::Device<Bound>,
    bar: &'a Bar0<'a>,
    #[pin]
    irq_test_completion: Completion,
    #[pin]
    irq_dma_completion: Completion,
}

impl pci::Driver for EduDriver {
    type IdInfo = ();
    type Data<'bound> = EduDriverData<'bound>;

    const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;

    fn probe<'bound>(
        pdev: &'bound pci::Device<kernel::device::Core<'_>>,
        _id_info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound {
        try_pin_init!(EduDriverData {
            _: {
                dev_dbg!(
                    pdev,
                    "Probe Rust EDU driver sample (PCI ID: {}, 0x{:x}).\n",
                    pdev.vendor_id(),
                    pdev.device_id(),
                );
            },

            _enable: pdev.enable_device().inspect(|_| pdev.set_master())?,

            bar: pdev.iomap_region_sized::<{ regs::END }>(0, c"rust_driver_edu")?,

            _irq: pdev.alloc_irq_vectors(1, 1, IrqTypes::default().with(pci::IrqType::Msi))?,

            irq_handler <- irq::Registration::new(
                _irq.index(0)?.into(),
                Flags::TRIGGER_NONE,
                c"rust_edu_irq",
                try_pin_init!(IrqHandler {
                    bar,
                    irq_test_completion <- Completion::new(),
                    irq_dma_completion <- Completion::new(),
                    pdev,
                }),
            ),

            dma: {
                let mask = DmaMask::new::<28>();

                // SAFETY: There are no concurrent calls to DMA allocation and
                // mapping primitives.
                unsafe { pdev.dma_set_mask_and_coherent(mask)? };

                Coherent::zeroed(pdev.as_ref(), GFP_KERNEL)
            }?,

            pdev,
        })
        .pin_chain(|this| this.selftest())
    }
}

impl irq::Handler for IrqHandler<'_> {
    fn handle(&self) -> irq::IrqReturn {
        dev_dbg!(self.pdev, "irq handler called\n");
        let status: u32 = self.bar.read(regs::IRQ_STATUS).into();

        // DMA_IRQ
        if status & DMA_IRQ != 0 {
            dev_dbg!(self.pdev, "handling dma completion in irq\n");
            self.bar.write(regs::IRQ_ACK, DMA_IRQ.into());
            self.irq_dma_completion.complete();
        }

        // TEST_IRQ
        let magic = status & !DMA_IRQ;
        if magic == IRQ_MAGIC_VALUE {
            dev_dbg!(self.pdev, "handling test completion in irq\n");
            self.bar.write(regs::IRQ_ACK, magic.into());
            self.irq_test_completion.complete();
        }

        irq::IrqReturn::Handled
    }
}

#[pinned_drop]
impl PinnedDrop for EduDriverData<'_> {
    fn drop(self: Pin<&mut Self>) {
        dev_dbg!(self.pdev, "Remove Rust EDU driver sample.\n");
    }
}

impl EduDriverData<'_> {
    fn selftest(&self) -> Result {
        self.config_space();
        self.magic()?;
        self.liveness_check()?;
        self.factorial()?;
        self.test_irq()?;
        self.test_dma()?;

        dev_info!(self.pdev, "rust_driver_edu tests successful\n");
        Ok(())
    }

    fn config_space(&self) {
        let pdev = self.pdev;
        let config = pdev.config_space();

        // Some PCI configuration space registers.
        register! {
            VENDOR_ID(u16) @ 0x0 {
                15:0 vendor_id;
            }

            REVISION_ID(u8) @ 0x8 {
                7:0 revision_id;
            }

            BAR(u32)[6] @ 0x10 {
                31:0 value;
            }
        }

        dev_info!(
            pdev,
            "config space read8 rev ID: {:x}\n",
            config.read(REVISION_ID).revision_id()
        );

        dev_info!(
            pdev,
            "config space read16 vendor ID: {:x}\n",
            config.read(VENDOR_ID).vendor_id()
        );

        dev_info!(
            pdev,
            "config space read32 BAR 0: {:x}\n",
            config.read(BAR::at(0)).value()
        );
    }

    fn magic(&self) -> Result {
        let pdev = self.pdev;
        let bar = &self.bar;
        let identification = bar.read(regs::IDENTIFICATION);

        let magic: u8 = identification.magic().into();

        if magic != QEMU_EDU_DEVICE_MAGIC {
            dev_err!(
                pdev,
                "magic mismatch: expected {:#x} got {:#x}\n",
                QEMU_EDU_DEVICE_MAGIC,
                magic
            );
            return Err(ENODEV);
        }

        dev_info!(
            pdev,
            "major: {:#x} minor: {:#x}\n",
            identification.major(),
            identification.minor()
        );
        Ok(())
    }

    fn liveness_check(&self) -> Result {
        let pdev = self.pdev;
        let bar = &self.bar;
        let test_value = 0xabcd;

        bar.write(regs::LIVENESS_CHECK, test_value.into());

        let inverse_value = bar.read(regs::LIVENESS_CHECK).into_raw();

        if inverse_value != !test_value {
            dev_err!(
                pdev,
                "inverse mismatch: expected {:#x} got {:#x}\n",
                !test_value,
                inverse_value
            );
            return Err(ENODEV);
        }

        dev_info!(pdev, "inverse test successful\n");
        Ok(())
    }

    fn factorial(&self) -> Result {
        let pdev = self.pdev;
        let bar = &self.bar;

        self.wait_until_compute_has_finished()?;

        bar.write(regs::FACTORIAL, 4.into());

        self.wait_until_compute_has_finished()?;

        let result: u32 = bar.read(regs::FACTORIAL).into();

        let expected = 24;

        if result != expected {
            dev_err!(
                pdev,
                "factorial result wrong: expected {} got {}\n",
                expected,
                result
            );
            return Err(ENODEV);
        }

        dev_info!(pdev, "factorial test successful\n");
        Ok(())
    }

    fn test_irq(&self) -> Result {
        let pdev = self.pdev;
        let bar = &self.bar;
        let handler = self.irq_handler().handler();

        dev_dbg!(pdev, "raising irq\n");

        bar.write(regs::IRQ_RAISE, IRQ_MAGIC_VALUE.into());

        handler.irq_test_completion.wait_for_completion();

        dev_info!(pdev, "irq test successful\n");
        Ok(())
    }

    fn test_dma(&self) -> Result {
        let pdev = self.pdev;
        let bar = &self.bar;
        let dma = &self.dma;
        let handler = self.irq_handler().handler();

        dev_dbg!(pdev, "testing dma\n");

        const DMA_VALUE: u64 = 42;

        io_write!(&dma, , DMA_VALUE);

        bar.write(regs::DMA_SRC, dma.dma_address().into());
        bar.write(regs::DMA_DST, QEMU_DMA_BASE.into());
        bar.write(regs::DMA_COUNT, (dma.size() as u64).into());
        bar.write(
            regs::DMA_COMMAND,
            regs::DMA_COMMAND::zeroed()
                .with_start_transfer(true)
                .with_direction(false)
                .with_raise_irq(true),
        );

        handler.irq_dma_completion.wait_for_completion();

        // Destroy previous value to test roundtrip
        io_write!(&dma, , 0u64);

        bar.write(regs::DMA_SRC, QEMU_DMA_BASE.into());
        bar.write(regs::DMA_DST, dma.dma_address().into());
        bar.write(regs::DMA_COUNT, (dma.size() as u64).into());
        bar.write(
            regs::DMA_COMMAND,
            regs::DMA_COMMAND::zeroed()
                .with_start_transfer(true)
                .with_direction(true)
                .with_raise_irq(true),
        );

        handler.irq_dma_completion.wait_for_completion();

        let result: u64 = io_read!(&dma,);

        if result != DMA_VALUE {
            dev_err!(
                pdev,
                "dma result wrong: expected {} got {}\n",
                DMA_VALUE,
                result
            );
            return Err(ENODEV);
        }

        dev_info!(pdev, "dma test successful\n");
        Ok(())
    }

    fn wait_until_compute_has_finished(&self) -> Result {
        let pdev = self.pdev;
        let bar = &self.bar;

        read_poll_timeout(
            || Ok(bar.read(regs::STATUS)),
            |status| status.computing() == 0,
            Delta::from_millis(10),
            Delta::from_millis(100),
        )
        .inspect_err(|_| dev_err!(pdev, "computation bit did not clear before timeout\n"))
        .map(|_| ())
    }
}

kernel::module_pci_driver! {
    type: EduDriver,
    name: "rust_driver_edu",
    authors: ["Maurice Hieronymus"],
    description: "Rust EDU driver",
    license: "GPL v2",
}
