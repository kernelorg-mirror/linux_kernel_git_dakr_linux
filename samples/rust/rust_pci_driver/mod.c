// SPDX-License-Identifier: GPL-2.0

#include <linux/module.h>
#include <linux/pci.h>

#define PCI_DEVICE_ID_REDHAT_QEMU_PCI_TESTDEV	0x0005

extern int rust_pci_driver_probe(struct pci_dev *dev, const struct pci_device_id *id);
extern void rust_pci_driver_remove(struct pci_dev *dev);

static const struct pci_device_id rust_pci_driver_ids[] = {
	{
		PCI_DEVICE(PCI_VENDOR_ID_REDHAT, PCI_DEVICE_ID_REDHAT_QEMU_PCI_TESTDEV)
	},
	{}
};

static struct pci_driver
rust_pci_driver = {
	.name = "rust_pci_driver_sample",
	.id_table = rust_pci_driver_ids,
	.probe = rust_pci_driver_probe,
	.remove = rust_pci_driver_remove,
};

static int __init rust_pci_driver_init(void)
{
	pr_info("%s\n", __func__);
	return pci_register_driver(&rust_pci_driver);
}

static void __exit rust_pci_driver_exit(void)
{
	pr_info("%s\n", __func__);
	pci_unregister_driver(&rust_pci_driver);
}

module_init(rust_pci_driver_init);
module_exit(rust_pci_driver_exit);

MODULE_AUTHOR("Danilo Krummrich");
MODULE_DESCRIPTION("Rust PCI driver sample");
MODULE_LICENSE("GPL v2");
