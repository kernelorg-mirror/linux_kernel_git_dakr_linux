// SPDX-License-Identifier: GPL-2.0

//! Nova-core auxbus data. Contains all the methods used by the auxbus drivers
//! to interact with nova-core.

use crate::gpu::Chipset;
use crate::gpu::Gpu;

/// Auxiliary bus registration data. Used by the auxbus drivers to call methods on
/// the GPU.
pub struct AuxData<'bound> {
    pub(crate) gpu: &'bound Gpu<'bound>,
}

impl AuxData<'_> {
    /// Returns the chipset of this GPU.
    pub fn chipset(&self) -> Chipset {
        self.gpu.spec.chipset
    }
}
