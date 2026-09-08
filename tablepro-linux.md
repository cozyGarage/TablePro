# Linux review: historical plan retired

The earlier divergence review in this file was superseded on 2026-09-07. It contained obsolete file-splitting, panic-guard, connection-organization, branch-tip and release-profile tasks. Git history preserves that review.

Use [PLAN.md](PLAN.md) for sequencing, [the stabilization audit](linux/docs/stabilization-2026-09.md) for current evidence and open findings, and [upstream adoption](linux/docs/upstream-adoption.md) for the whole-app comparison through macOS 0.72.

Do not automatically introduce fat LTO, panic-abort, a new branch, or broad file splitting from the retired plan. Optimization requires measurements and existing safety contracts remain in force.
