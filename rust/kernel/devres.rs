// SPDX-License-Identifier: GPL-2.0

//! Devres abstraction
//!
//! [`Devres`] represents an abstraction for the kernel devres (device resource management)
//! implementation.

use crate::{
    alloc::Flags,
    bindings,
    device::{
        Bound,
        Device, //
    },
    error::to_result,
    new_spinlock,
    prelude::*,
    revocable::{
        Revocable,
        RevocableGuard, //
    },
    sync::{
        aref::ARef,
        rcu,
        Arc,
        SpinLock, //
    },
    types::{
        ForLt,
        ForeignOwnable,
        Opaque, //
    },
};

/// Tracks how many chained [`DevresChain`] instances depend on a resource, and whether the
/// standalone owner deferred its revocation to the last chain.
struct ChainState {
    /// Number of chained [`DevresChain`] instances that depend on this resource.
    count: i32,
    /// Set when the standalone owner's [`Drop`] skipped revocation due to `count > 0`.
    deferred: bool,
}

/// Inner type that embeds a `struct devres_node` and the `Revocable<T>`.
///
/// When used with a dependency (`D != ()`), `dep` holds a reference to the dependency's inner
/// state.
#[repr(C)]
#[pin_data]
struct Inner<T, D = ()> {
    #[pin]
    node: Opaque<bindings::devres_node>,
    dep: Option<Arc<Inner<D>>>,
    #[pin]
    data: Revocable<T>,
    #[pin]
    chain_state: SpinLock<ChainState>,
}

/// This abstraction is meant to be used by subsystems to containerize [`Device`] bound resources to
/// manage their lifetime.
///
/// [`Device`] bound resources should be freed when either the resource goes out of scope or the
/// [`Device`] is unbound respectively, depending on what happens first. In any case, it is always
/// guaranteed that revoking the device resource is completed before the corresponding [`Device`]
/// is unbound.
///
/// To achieve that [`Devres`] registers a devres callback on creation, which is called once the
/// [`Device`] is unbound, revoking access to the encapsulated resource (see also [`Revocable`]).
///
/// After the [`Devres`] has been unbound it is not possible to access the encapsulated resource
/// anymore.
///
/// [`Devres`] users should make sure to simply free the corresponding backing resource in `T`'s
/// [`Drop`] implementation.
///
/// See also [`DevresChain`] for resources that depend on another [`Devres`]-managed resource.
///
/// # Examples
///
/// ```no_run
/// use kernel::{
///     bindings,
///     device::{
///         Bound,
///         Device, //
///     },
///     devres::Devres,
///     io::{
///         Io,
///         IoKnownSize,
///         Mmio,
///         MmioRaw,
///         PhysAddr, //
///     },
///     prelude::*,
/// };
/// use core::ops::Deref;
///
/// // See also [`pci::Bar`] for a real example.
/// struct IoMem<const SIZE: usize>(MmioRaw<SIZE>);
///
/// impl<const SIZE: usize> IoMem<SIZE> {
///     /// # Safety
///     ///
///     /// [`paddr`, `paddr` + `SIZE`) must be a valid MMIO region that is mappable into the CPUs
///     /// virtual address space.
///     unsafe fn new(paddr: usize) -> Result<Self>{
///         // SAFETY: By the safety requirements of this function [`paddr`, `paddr` + `SIZE`) is
///         // valid for `ioremap`.
///         let addr = unsafe { bindings::ioremap(paddr as PhysAddr, SIZE) };
///         if addr.is_null() {
///             return Err(ENOMEM);
///         }
///
///         Ok(IoMem(MmioRaw::new(addr as usize, SIZE)?))
///     }
/// }
///
/// impl<const SIZE: usize> Drop for IoMem<SIZE> {
///     fn drop(&mut self) {
///         // SAFETY: `self.0.addr()` is guaranteed to be properly mapped by `Self::new`.
///         unsafe { bindings::iounmap(self.0.addr() as *mut c_void); };
///     }
/// }
///
/// impl<const SIZE: usize> Deref for IoMem<SIZE> {
///    type Target = Mmio<SIZE>;
///
///    fn deref(&self) -> &Self::Target {
///         // SAFETY: The memory range stored in `self` has been properly mapped in `Self::new`.
///         unsafe { Mmio::from_raw(&self.0) }
///    }
/// }
/// # fn no_run(dev: &Device<Bound>) -> Result<(), Error> {
/// // SAFETY: Invalid usage for example purposes.
/// let iomem = unsafe { IoMem::<{ core::mem::size_of::<u32>() }>::new(0xBAAAAAAD)? };
/// let devres = Devres::new(dev, iomem)?;
///
/// let res = devres.try_access().ok_or(ENXIO)?;
/// res.write8(0x42, 0x0);
/// # Ok(())
/// # }
/// ```
pub struct Devres<T: Send>(DevresChain<ForLt!(T)>);

// Calling the FFI functions from the `base` module directly from the `Devres<T>` impl may result in
// them being called directly from driver modules. This happens since the Rust compiler will use
// monomorphisation, so it might happen that functions are instantiated within the calling driver
// module. For now, work around this with `#[inline(never)]` helpers.
//
// TODO: Remove once a more generic solution has been implemented. For instance, we may be able to
// leverage `bindgen` to take care of this depending on whether a symbol is (already) exported.
mod base {
    use kernel::{
        bindings,
        prelude::*, //
    };

    #[inline(never)]
    #[allow(clippy::missing_safety_doc)]
    pub(super) unsafe fn devres_node_init(
        node: *mut bindings::devres_node,
        release: bindings::dr_node_release_t,
        free: bindings::dr_node_free_t,
    ) {
        // SAFETY: Safety requirements are the same as `bindings::devres_node_init`.
        unsafe { bindings::devres_node_init(node, release, free) }
    }

    #[inline(never)]
    #[allow(clippy::missing_safety_doc)]
    pub(super) unsafe fn devres_set_node_dbginfo(
        node: *mut bindings::devres_node,
        name: *const c_char,
        size: usize,
    ) {
        // SAFETY: Safety requirements are the same as `bindings::devres_set_node_dbginfo`.
        unsafe { bindings::devres_set_node_dbginfo(node, name, size) }
    }

    #[inline(never)]
    #[allow(clippy::missing_safety_doc)]
    pub(super) unsafe fn devres_node_add(
        dev: *mut bindings::device,
        node: *mut bindings::devres_node,
    ) {
        // SAFETY: Safety requirements are the same as `bindings::devres_node_add`.
        unsafe { bindings::devres_node_add(dev, node) }
    }

    #[must_use]
    #[inline(never)]
    #[allow(clippy::missing_safety_doc)]
    pub(super) unsafe fn devres_node_remove(
        dev: *mut bindings::device,
        node: *mut bindings::devres_node,
    ) -> bool {
        // SAFETY: Safety requirements are the same as `bindings::devres_node_remove`.
        unsafe { bindings::devres_node_remove(dev, node) }
    }
}

/// Like [`Devres`], but for resources that depend on another [`Devres`]-managed resource and
/// need access to it during teardown.
///
/// The resource type `F::Of<'a>` holds a direct `&'a D` reference to its dependency, so all
/// methods, including [`Drop`], can access it directly.
///
/// Use the [`ForLt!`] macro to associate the resource type with its [`trait@ForLt`] implementation.
///
/// # Invariants
///
/// - The devres node of the dependency `D` is always registered before this node on the same
///   [`Device`], ensuring this node is released first during device unbind.
/// - The dependency's chain count is incremented while this [`DevresChain`] is alive, preventing
///   the dependency's drop from revoking `D`.
/// - Together, these invariants guarantee that `D` is accessible whenever this resource has not
///   yet been revoked.
///
/// # Examples
///
/// ```ignore
/// use kernel::{
///     devres::DevresChain,
///     types::ForLt, //
/// };
///
/// struct MyResource<'a> {
///     dep: &'a Bar0,
/// }
///
/// impl Drop for MyResource<'_> {
///     fn drop(&mut self) {
///         // Can access self.dep directly -- guaranteed valid.
///     }
/// }
///
/// let res: DevresChain<ForLt!(MyResource<'_>), Bar0> =
///     DevresChain::new(dev, &bar_devres, |bar| Ok(MyResource { dep: bar }))?;
/// ```
pub struct DevresChain<F: ForLt, D: Send + 'static = ()>
where
    F::Of<'static>: Send,
{
    dev: ARef<Device>,
    inner: Arc<Inner<F::Of<'static>, D>>,
}

impl<T: Send> core::ops::Deref for Devres<T> {
    type Target = DevresChain<ForLt!(T)>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Send> Devres<T> {
    /// Creates a new [`Devres`] instance of the given `data`.
    ///
    /// The `data` encapsulated within the returned `Devres` instance' `data` will be
    /// (revoked)[`Revocable`] once the device is detached.
    pub fn new<E>(dev: &Device<Bound>, data: impl PinInit<T, E>) -> Result<Self>
    where
        Error: From<E>,
    {
        Ok(Self(DevresChain::new_internal(dev, data, None)?))
    }
}

impl<F: ForLt, D: Send + 'static> DevresChain<F, D>
where
    F::Of<'static>: Send,
{
    fn new_internal<E>(
        dev: &Device<Bound>,
        data: impl PinInit<F::Of<'static>, E>,
        dep: Option<Arc<Inner<D>>>,
    ) -> Result<Self>
    where
        Error: From<E>,
    {
        // TODO: Use `core::any::type_name::<T>()` once it is a `const fn`, such that we can
        // convert the `&str` to a `&CStr` at compile-time.
        let name = if dep.is_some() {
            c"DevresChain"
        } else {
            c"Devres"
        };

        let inner = Arc::pin_init::<Error>(
            try_pin_init!(Inner {
                node <- Opaque::ffi_init(|node: *mut bindings::devres_node| {
                    // SAFETY: `node` is a valid pointer to an uninitialized `struct devres_node`.
                    unsafe {
                        base::devres_node_init(
                            node,
                            Some(Self::devres_node_release),
                            Some(Self::devres_node_free_node),
                        )
                    };

                    // SAFETY: `node` is a valid pointer to an uninitialized `struct devres_node`.
                    unsafe {
                        base::devres_set_node_dbginfo(
                            node,
                            name.as_char_ptr(),
                            core::mem::size_of::<Revocable<F::Of<'static>>>(),
                        )
                    };
                }),
                dep,
                data <- Revocable::new(data),
                chain_state <- new_spinlock!(ChainState { count: 0, deferred: false }),
            }),
            GFP_KERNEL,
        )?;

        if let Some(dep_inner) = &inner.dep {
            dep_inner.chain_state.lock().count += 1;
        }

        // SAFETY:
        // - `dev` is a valid pointer to a bound `struct device`.
        // - `node` is a valid pointer to a `struct devres_node`.
        // - `devres_node_add()` is guaranteed not to call `devres_node_release()` for the entire
        //    lifetime of `dev`.
        unsafe { base::devres_node_add(dev.as_raw(), inner.node.get()) };

        // Take additional reference count for `devres_node_add()`.
        core::mem::forget(inner.clone());

        Ok(Self {
            dev: dev.into(),
            inner,
        })
    }

    /// Creates a new [`DevresChain`] instance that depends on a [`Devres`]-managed resource.
    ///
    /// `dep` is the [`Devres`]-managed dependency that the resource requires access to during
    /// teardown and regular operation. Both must belong to the same [`Device`].
    ///
    /// `f` receives a reference to the dependency's data and returns the resource to be managed.
    /// The returned resource type `F::Of<'a>` may hold the `&'a D` reference directly, so that
    /// [`Drop`] can access the dependency directly.
    pub fn new(
        dev: &Device<Bound>,
        dep: &Devres<D>,
        f: impl for<'a> FnOnce(&'a D) -> Result<F::Of<'a>>,
    ) -> Result<Self> {
        if dep.device().as_raw() != dev.as_raw() {
            return Err(EINVAL);
        }

        let dep_inner = dep.0.inner.clone();

        // SAFETY: `dev` is a `&Device<Bound>`, so the device hasn't been unbound yet and devres
        // hasn't released any nodes. Since `dep` is alive, its data hasn't been revoked.
        let dep_ref = unsafe { dep_inner.data.access() };

        // SAFETY: The chain invariants (devres LIFO ordering + chain_state coordination) guarantee
        // that D remains alive for as long as our Revocable data hasn't been revoked. Transmuting
        // to 'static is sound because we store the result in a Revocable that can only be accessed
        // while these invariants hold.
        let dep_ref: &'static D = unsafe { &*core::ptr::from_ref(dep_ref) };

        let data = f(dep_ref)?;

        Self::new_internal(dev, data, Some(dep_inner))
    }

    #[allow(clippy::missing_safety_doc)]
    unsafe extern "C" fn devres_node_release(
        _dev: *mut bindings::device,
        node: *mut bindings::devres_node,
    ) {
        let node = Opaque::cast_from(node);

        // SAFETY: `node` is in the same allocation as its container.
        let inner = unsafe { kernel::container_of!(node, Inner<F::Of<'static>, D>, node) };

        // SAFETY: `inner` is a valid `Inner<F::Of<'static>, D>` pointer.
        let inner = unsafe { &*inner };

        inner.data.revoke();

        if let Some(dep_inner) = &inner.dep {
            dep_inner.chain_state.lock().count -= 1;
        }
    }

    #[allow(clippy::missing_safety_doc)]
    unsafe extern "C" fn devres_node_free_node(node: *mut bindings::devres_node) {
        let node = Opaque::cast_from(node);

        // SAFETY: `node` is in the same allocation as its container.
        let inner = unsafe { kernel::container_of!(node, Inner<F::Of<'static>, D>, node) };

        // SAFETY: `inner` points to the entire `Inner<F::Of<'static>, D>` allocation.
        drop(unsafe { Arc::from_raw(inner) });
    }

    /// Try to remove a devres node and drop the extra [`Arc`] reference that was taken for
    /// `devres_node_add()` during construction.
    ///
    /// # Safety
    ///
    /// The caller must have successfully revoked the data associated with `inner`.
    unsafe fn try_remove_devres_node<U, V>(&self, inner: &Arc<Inner<U, V>>) {
        // SAFETY: The caller guarantees the data has been revoked.
        if unsafe { base::devres_node_remove(self.dev.as_raw(), inner.node.get()) } {
            // SAFETY: The constructor took an additional reference count for
            // `devres_node_add()`. Since removal succeeded, drop it.
            drop(unsafe { Arc::from_raw(Arc::as_ptr(inner)) });
        }
    }

    /// Return a reference of the [`Device`] this [`DevresChain`] instance has been created with.
    pub fn device(&self) -> &Device {
        &self.dev
    }

    /// Obtain `&'a F::Of<'a>`, bypassing the [`Revocable`].
    ///
    /// This method allows to directly obtain a reference to the managed data, bypassing the
    /// [`Revocable`], by presenting a `&'a Device<Bound>` of the same [`Device`] this instance
    /// has been created with.
    ///
    /// # Errors
    ///
    /// An error is returned if `dev` does not match the same [`Device`] this instance has been
    /// created with.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// #![cfg(CONFIG_PCI)]
    /// use kernel::{
    ///     device::Core,
    ///     devres::Devres,
    ///     io::{
    ///         Io,
    ///         IoKnownSize, //
    ///     },
    ///     pci, //
    /// };
    ///
    /// fn from_core(dev: &pci::Device<Core>, devres: Devres<pci::Bar<0x4>>) -> Result {
    ///     let bar = devres.access(dev.as_ref())?;
    ///
    ///     let _ = bar.read32(0x0);
    ///
    ///     // might_sleep()
    ///
    ///     bar.write32(0x42, 0x0);
    ///
    ///     Ok(())
    /// }
    /// ```
    pub fn access<'a>(&'a self, dev: &'a Device<Bound>) -> Result<&'a F::Of<'a>> {
        if self.dev.as_raw() != dev.as_raw() {
            return Err(EINVAL);
        }

        // SAFETY: `dev` being the same device as the device this instance has been created for
        // proves that the data hasn't been revoked and is guaranteed to not be revoked as long
        // as `dev` lives; `dev` lives at least as long as `self`.
        Ok(F::cast_ref(unsafe { self.inner.data.access() }))
    }

    /// [`DevresChain`] accessor for [`Revocable::try_access`].
    pub fn try_access(&self) -> Option<DevresGuard<'_, F>> {
        self.inner
            .data
            .try_access()
            .map(|guard| DevresGuard { inner: guard })
    }

    /// [`DevresChain`] accessor for [`Revocable::try_access_with`].
    pub fn try_access_with<R, G>(&self, f: G) -> Option<R>
    where
        G: for<'a> FnOnce(&'a F::Of<'a>) -> R,
    {
        self.inner.data.try_access_with(|data| f(F::cast_ref(data)))
    }

    /// [`DevresChain`] accessor for [`Revocable::try_access_with_guard`].
    pub fn try_access_with_guard<'a>(&'a self, guard: &'a rcu::Guard) -> Option<&'a F::Of<'a>> {
        self.inner
            .data
            .try_access_with_guard(guard)
            .map(|data| F::cast_ref(data))
    }
}

// SAFETY: `DevresChain` can be sent to any task, if `F::Of<'static>: Send` and `D: Send`.
unsafe impl<F: ForLt, D: Send + 'static> Send for DevresChain<F, D> where F::Of<'static>: Send {}

// SAFETY: `DevresChain` can be shared with any task, if `F::Of<'static>: Send + Sync` and
// `D: Send + Sync`.
unsafe impl<F: ForLt, D: Send + Sync + 'static> Sync for DevresChain<F, D> where
    F::Of<'static>: Send + Sync
{
}

/// Guard returned by [`DevresChain::try_access`].
pub struct DevresGuard<'a, F: ForLt> {
    inner: RevocableGuard<'a, F::Of<'static>>,
}

impl<'a, F: ForLt> core::ops::Deref for DevresGuard<'a, F> {
    type Target = F::Of<'a>;

    fn deref(&self) -> &Self::Target {
        F::cast_ref(&*self.inner)
    }
}

impl<F: ForLt, D: Send + 'static> Drop for DevresChain<F, D>
where
    F::Of<'static>: Send,
{
    fn drop(&mut self) {
        // Standalone with active chains: defer revocation so the last chain to drop will
        // revoke on our behalf. If no chain does, devres will revoke during device unbind.
        if self.inner.dep.is_none() {
            let mut state = self.inner.chain_state.lock();
            if state.count > 0 {
                state.deferred = true;
                return;
            }
        }

        // SAFETY: When `drop` runs, it is guaranteed that nobody is accessing the revocable
        // data anymore, hence it is safe not to wait for the grace period to finish.
        if !unsafe { self.inner.data.revoke_nosync() } {
            return;
        }

        // SAFETY: We successfully revoked our data above.
        unsafe { self.try_remove_devres_node(&self.inner) };

        // Chained: update the dependency's chain state and possibly finalize it.
        if let Some(dep_inner) = &self.inner.dep {
            let should_revoke_dep = {
                let mut state = dep_inner.chain_state.lock();
                state.count -= 1;
                state.count == 0 && state.deferred
            };

            if should_revoke_dep {
                // SAFETY: The standalone `Devres<D>` has been dropped (`deferred` is set),
                // so all Rust references to `D` through the `Devres` API have been released.
                if unsafe { dep_inner.data.revoke_nosync() } {
                    // SAFETY: We successfully revoked the dependency's data.
                    unsafe { self.try_remove_devres_node(dep_inner) };
                }
            }
        }
    }
}

/// Consume `data` and [`Drop::drop`] `data` once `dev` is unbound.
fn register_foreign<P>(dev: &Device<Bound>, data: P) -> Result
where
    P: ForeignOwnable + Send + 'static,
{
    let ptr = data.into_foreign();

    #[allow(clippy::missing_safety_doc)]
    unsafe extern "C" fn callback<P: ForeignOwnable>(ptr: *mut kernel::ffi::c_void) {
        // SAFETY: `ptr` is the pointer to the `ForeignOwnable` leaked above and hence valid.
        drop(unsafe { P::from_foreign(ptr.cast()) });
    }

    // SAFETY:
    // - `dev.as_raw()` is a pointer to a valid and bound device.
    // - `ptr` is a valid pointer the `ForeignOwnable` devres takes ownership of.
    to_result(unsafe {
        // `devm_add_action_or_reset()` also calls `callback` on failure, such that the
        // `ForeignOwnable` is released eventually.
        bindings::devm_add_action_or_reset(dev.as_raw(), Some(callback::<P>), ptr.cast())
    })
}

/// Encapsulate `data` in a [`KBox`] and [`Drop::drop`] `data` once `dev` is unbound.
///
/// # Examples
///
/// ```no_run
/// use kernel::{
///     device::{
///         Bound,
///         Device, //
///     },
///     devres, //
/// };
///
/// /// Registration of e.g. a class device, IRQ, etc.
/// struct Registration;
///
/// impl Registration {
///     fn new() -> Self {
///         // register
///
///         Self
///     }
/// }
///
/// impl Drop for Registration {
///     fn drop(&mut self) {
///        // unregister
///     }
/// }
///
/// fn from_bound_context(dev: &Device<Bound>) -> Result {
///     devres::register(dev, Registration::new(), GFP_KERNEL)
/// }
/// ```
pub fn register<T, E>(dev: &Device<Bound>, data: impl PinInit<T, E>, flags: Flags) -> Result
where
    T: Send + 'static,
    Error: From<E>,
{
    let data = KBox::pin_init(data, flags)?;

    register_foreign(dev, data)
}
