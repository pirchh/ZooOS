# Kickoff prompt for Claude

Read README.md and AGENTS.md, then ARCHITECTURE.md, DECISIONS.md, BUILD.md,
MILESTONES.md and COMPATIBILITY.md in this repository.

Let's get after it: implement milestones M0 and M1 for this independent Rust-first
OS. (Historical kickoff prompt. The project has since been renamed from the
placeholder OriginsOS to ZooOS; it is not Stonepyre. Use zoo-os as the identifier.) Work autonomously through routine implementation choices
within those milestones and record decisions as you go.

Use an established UEFI bootloader, an x86-64 freestanding Rust kernel, and QEMU.
Deliver the workspace and build tooling, version pins, firmware setup instructions,
serial startup output, deliberate panic mode, and bounded verification harness
specified in the handoff. Prove that kernel code actually executes in the guest.
Run normal, panic and negative verification cases and report the actual results.

The long-term direction is a macOS-inspired desktop, a local assistant with safe
file actions, Windows compatibility targeting Opera/Battle.net/WoW, and later an
iPhone bridge. Do not implement or scaffold those future features in this run.
Do not substitute Windows or Linux as the guest kernel foundation.

Keep unsafe code small and explain its assumptions. Verify dependency/toolchain
compatibility rather than guessing. Do not change host boot settings, touch real
disks, install global dependencies without required authorization, or claim tests
passed when they could not run. If a prerequisite blocks execution, state exactly
what is missing and finish independent work without inventing verification.

Stop after M1. Return a concise report of the files created, exact commands run,
observed outcomes, remaining blockers and the next recommended milestone.

