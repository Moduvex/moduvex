//! Compile-time dependency satisfaction traits.
//!
//! The registered module list is encoded as a nested-tuple type:
//!   Empty: `()`  |  One: `(A, ())`  |  Two: `(A, (B, ()))`
//!
//! `.module::<M>(instance)` prepends M to the head, yielding `(M, Prev)`.
//!
//! # How dependency checking works
//!
//! `AllDepsOk` is the bound on `.run()`. It carries a `Proofs` type parameter
//! that the compiler infers automatically — users never name it.
//!
//! # Coherence strategy
//!
//! We use proof witnesses (`Here`, `There<P>`) to distinguish the base case
//! (M is the head) from the recursive case (M is in the tail) in
//! `ContainsModule`, avoiding overlapping-impl coherence errors.
//!
//! `AllDepsOk<Proofs>` and `ContainsAll<Required, Proofs>` both carry `Proofs`
//! as an explicit parameter so every type param in every impl is constrained.
//! The `.run()` method on the builder uses `Modules: AllDepsOk<P>` where `P`
//! is a free type parameter — the compiler infers P automatically.
//!
//! # Recursion limit
//! Default 128; add `#![recursion_limit = "256"]` for >60 modules.

use std::marker::PhantomData;

// ── Proof witnesses ───────────────────────────────────────────────────────────

/// Proof: the target module is at the *head* of the list.
pub struct Here;

/// Proof: the target module is somewhere in the *tail*, witnessed by `P`.
pub struct There<P>(PhantomData<P>);

// ── ContainsModule ────────────────────────────────────────────────────────────

/// `List: ContainsModule<M, Proof>` — M is in List, witnessed by Proof.
pub trait ContainsModule<M, Proof> {}

// Base case: M is the head.
impl<M, Rest> ContainsModule<M, Here> for (M, Rest) {}

// Recursive case: M is somewhere in the tail.
impl<M, Head, Rest, P> ContainsModule<M, There<P>> for (Head, Rest)
where
    Rest: ContainsModule<M, P>,
{
}

// ── DependsOn ─────────────────────────────────────────────────────────────────

/// Declares the compile-time dependencies of a module.
///
/// Set `Required` to the nested-tuple of required module types, or `()` for
/// modules with no dependencies.
///
/// # Example
/// ```rust,ignore
/// impl DependsOn for UserModule {
///     type Required = (DatabaseModule, ());
/// }
/// ```
pub trait DependsOn {
    /// Nested-tuple type-list of required modules, e.g. `(DbModule, ())`.
    type Required;
}

// ── ContainsAll ───────────────────────────────────────────────────────────────

/// "All types in `Required` are present in `List`", witnessed by `Proofs`.
///
/// `Required` and `Proofs` are parallel nested tuples.
pub trait ContainsAll<Required, Proofs> {}

impl<List> ContainsAll<(), ()> for List {}

impl<List, Head, HeadProof, Tail, TailProofs>
    ContainsAll<(Head, Tail), (HeadProof, TailProofs)> for List
where
    List: ContainsModule<Head, HeadProof>,
    List: ContainsAll<Tail, TailProofs>,
{
}

// ── AllDepsOk ─────────────────────────────────────────────────────────────────

/// "Every module in the list has all its dependencies also in the list."
///
/// `Proofs` is a nested tuple of proof witnesses — one per module. The
/// compiler infers it automatically; users never name this parameter.
///
/// This is the bound placed on `.run()`. The builder method takes a free type
/// parameter `P` and requires `Modules: AllDepsOk<P>`, which lets the solver
/// find the proof without the user specifying it.
pub trait AllDepsOk<Proofs> {}

// Empty list.
impl AllDepsOk<()> for () {}

// Recursive: M's deps are in (M, Rest) (proven by MProofs),
// and Rest is consistent (proven by RestProofs).
impl<M, Rest, MProofs, RestProofs> AllDepsOk<(MProofs, RestProofs)> for (M, Rest)
where
    M: DependsOn,
    Rest: AllDepsOk<RestProofs>,
    (M, Rest): ContainsAll<M::Required, MProofs>,
{
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct ModA;
    struct ModB;
    struct ModC;

    impl DependsOn for ModA { type Required = (); }
    impl DependsOn for ModB { type Required = (ModA, ()); }
    impl DependsOn for ModC { type Required = (ModA, (ModB, ())); }

    fn assert_ok<L, P>() where L: AllDepsOk<P> {}

    #[test]
    fn empty_list_ok() {
        fn check() { assert_ok::<(), ()>(); }
        check();
    }

    #[test]
    fn single_no_dep_ok() {
        fn check() { assert_ok::<(ModA, ()), _>(); }
        check();
    }

    #[test]
    fn two_modules_with_dep_ok() {
        fn check() { assert_ok::<(ModB, (ModA, ())), _>(); }
        check();
    }

    #[test]
    fn three_modules_multi_dep_ok() {
        fn check() { assert_ok::<(ModC, (ModB, (ModA, ()))), _>(); }
        check();
    }
}
