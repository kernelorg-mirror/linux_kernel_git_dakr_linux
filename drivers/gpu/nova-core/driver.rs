// SPDX-License-Identifier: GPL-2.0

use kernel::{
    auxiliary,
    device::Core,
    pci,
    pci::{
        Class,
        ClassMask,
        Vendor, //
    },
    prelude::*,
    sync::atomic::{
        Atomic,
        Relaxed, //
    },
    types::ForLt,
};

use crate::{
    api::NovaCoreApi,
    gpu::{
        Gpu,
        BAR0_SIZE, //
    }, //
};

/// Counter for generating unique auxiliary device IDs.
static AUXILIARY_ID_COUNTER: Atomic<u32> = Atomic::new(0);

#[pin_data]
pub(crate) struct NovaCore<'bound> {
    #[allow(clippy::type_complexity)]
    #[not_covariant]
    _reg: auxiliary::Registration<'gpu, ForLt!(NovaCoreApi<'_>)>,
    #[pin]
    pub(crate) gpu: Gpu<'bound>,
    _enable: pci::DeviceEnableGuard<'bound>,
}

pub(crate) struct NovaCoreDriver;

kernel::pci_device_table!(
    PCI_TABLE,
    <NovaCoreDriver as pci::Driver>::IdInfo,
    [
        // Modern NVIDIA GPUs will show up as either VGA or 3D controllers.
        (
            pci::DeviceId::from_class_and_vendor(
                Class::DISPLAY_VGA,
                ClassMask::ClassSubclass,
                Vendor::NVIDIA
            ),
            ()
        ),
        (
            pci::DeviceId::from_class_and_vendor(
                Class::DISPLAY_3D,
                ClassMask::ClassSubclass,
                Vendor::NVIDIA
            ),
            ()
        ),
    ]
);

impl pci::Driver for NovaCoreDriver {
    type IdInfo = ();
    type Data<'bound> = NovaCore<'bound>;
    const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;

    fn probe<'bound>(
        pdev: &'bound pci::Device<Core<'_>>,
        _info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound {
        pin_init::pin_init_scope(move || {
            dev_dbg!(pdev, "Probe Nova Core GPU driver.\n");

            let enable = pdev.enable_device()?;
            pdev.set_master();

            Ok(try_pin_init!(NovaCore {
                gpu <- Gpu::new(pdev, pdev.iomap_region_sized::<BAR0_SIZE>(0, c"nova-core/bar0")?),
                // SAFETY: `NovaCore` is dropped when the device is unbound;
                // i.e. `mem::forget()` is never called on it.
                _reg: unsafe {
                    auxiliary::Registration::new_with_lt(
                        pdev.as_ref(),
                        c"nova-drm",
                        // TODO[XARR]: Use XArray or perhaps IDA for proper ID
                        // allocation/recycling. For now, use a simple atomic counter that
                        // never recycles IDs.
                        AUXILIARY_ID_COUNTER.fetch_add(1, Relaxed),
                        crate::MODULE_NAME,
                        NovaCoreApi { gpu: gpu.get_ref(), pdev },
                    )?
                },
                _enable: enable,
            }))
        })
    }
}
