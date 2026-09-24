# Revert kit: the handle-table / handle-sweeper arm

For the (a)-vs-(b) discriminator described in `../lowmem768-panic-analysis.md`. Built 2026-09-12
while the floor was llama's; **not yet applied or compiled.**

Contents are the *feature-on* state, captured so the arm can be reconstructed deterministically
even if the working tree moves: the five affected files verbatim, plus `feature-on.diff` (the
tracked hunks against HEAD).

## What the arm reverts

| file | action | why |
|---|---|---|
| `handlecache.rs` (reference runtime) | full revert to HEAD | the published-handle table itself |
| `handlesweep.rs` (monitor) | move aside — **untracked**, so `git checkout` will not remove it | the sweeper |
| `mod.rs` | revert **2 hunks only**: `pub mod handlesweep;` and the `HandleSweeper::new()` call | the wiring |
| `space.rs` | full revert (`dump_handle_counts`) | diagnostic, called only from the reclaim path; dead without the sweeper |
| `runcomp.rs` | revert **hunk 1 only** (`mapped_object_count`) | diagnostic |

## What the arm MUST NOT revert

**`runcomp.rs` hunk 2 — the field-granular comp-config write — stays in BOTH arms.** It replaces a
`read_comp_config` / `write_config` round trip with a single atomic `set_tls_template` store. The
whole-struct rewrite races *every* atomic in the config: it can lose a concurrent `post_signal`
`fetch_or`, and it clobbers the compartment-written `handle_table`.

Only the second of those is about my feature. **The `post_signal` race predates it and is
independent of it**, so reverting this hunk would make the feature-off arm worse than baseline in
a way that has nothing to do with the hypothesis — and any resulting flakiness would read as
"reverting the sweeper didn't help", which is exactly the wrong conclusion.

**`state.rs` stays too.** It is untracked but `mon/mod.rs:42` in *HEAD* declares `pub mod state;`,
so removing it does not build. It belongs to different work; see the repo-level note below.

## Trap this kit exists to avoid

Reverting `mod.rs` wholesale looks right and is wrong twice: it removes `pub mod state;` along with
the handlesweep wiring, producing an arm that is both multi-variable and non-compiling. The two
handlesweep hunks are the only ones to touch.

## Repo-level finding, not ours to fix

HEAD does not build from a clean checkout. `mon/mod.rs:42` (`pub mod state;`), `twizzler-io`'s
`pub(crate) mod intr;`, and four workspace members named in the root `Cargo.toml`
(ssh, ctl, brush, ports/brush) all reference files that were never committed — cargo cannot even
parse the workspace. Found while building this kit; verified and extended by twizzler-0a, who
preserved the critical untracked sources into
`target/results/uncommitted-delta-20260912/untracked-src/` and surfaced it to Daniel. Committing
another author's files is worse than reporting them, so neither of us did.

## Still to do before the arm is trustworthy

Apply, `cargo check-all`, and confirm it compiles. An arm that fails to build is discovered at the
worst possible moment, and this one has a known hazard (`state.rs`) that a careless revert trips.
