# Agent instructions — independent OS

This is the ZooOS project (renamed from the placeholder OriginsOS on 2026-10-30). Use ZooOS as the name and zoo-os for technical identifiers.
Components follow a zoo theme: the main runtime/service supervisor is Zookeeper and individual services or workers are animal-named handlers (e.g. BelugaTest). Use these names only for components that actually exist.
Read all handoff documents before editing. User instructions take precedence.

## Authorized scope

The initial task authorizes M0 and M1 only. Implement the smallest working system
that meets their acceptance criteria. Do not add the desktop, application
compatibility, model runtime, phone bridge, scheduler, or filesystem prematurely.
Reuse an established bootloader rather than write one.

Work only in this project's directory. Treat any parent sources/ directory as
read-only. Do not alter the Stonepyre project, unrelated files, host boot settings,
physical disks, or firmware. Never test by installing on the user's real machine.

**Owner rule, absolute (2026-10-30):** never boot, install or test ZooOS on the
development machine (the Windows PC this repository lives on), whether from
its disks or from USB, and never overwrite, repartition, change boot settings
or otherwise modify its Windows installation or disks. On this machine only
QEMU is allowed. Real-hardware tests run only on the owner's other machines,
booted from a USB stick; any image written to USB must target a device the
owner explicitly names, never an internal disk.
Do not install global dependencies or change host configuration without the
permissions required by the environment. Repository-local dependencies and
reversible project work are within scope.

## Implementation standards

- Rust first, minimal assembly. Document each unsafe block's safety assumptions.
- No allocator, unwinding, or standard library in the M1 kernel.
- Pin dependency and compiler versions after verifying compatibility. Commit the
  application workspace Cargo.lock. No floating Git branches or unpinned nightly.
- Use explicit error messages, bounded waits, and subprocess cleanup.
- Preserve unrelated changes. Do not use destructive reset or cleanup commands.
- Keep architecture documents synchronized with actual decisions; label proposals
  and unimplemented features. Do not present a host mockup as an OS implementation.
- Do not claim performance improvements without reproducible measurements.

## Verification and reporting

Build and run the actual QEMU guest. Host print statements, invented screenshots,
placeholder functions, and hardcoded successful test results are not evidence.
Verify the normal boot and deliberate panic cases independently. Include a
negative verification case that proves the verifier rejects missing markers or
a timeout. Keep logs, versions, commands and outcomes in docs/verification.md.
Generated images, firmware, target output and logs must not be checked into Git
unless there is a specific reason; document how they are reproduced.

If a required tool or download is unavailable, finish independent work and report
the exact blocker. Do not call an unrun check passed. Do not switch kernel
foundations or silently replace the target to avoid a blocker. Routine choices
within the written scope can be made autonomously and recorded in DECISIONS.md.
Escalate a decision only when it changes scope, requires host privileges, or
invalidates an agreed constraint.

Stop after M1 and summarize files changed, exact checks and results, limitations,
and the next milestone. Later milestones need a subsequent instruction.

