# Start here

This is the implementation handoff for the OriginsOS project (placeholder name). It is not
Stonepyre. Copy this entire folder into its own new project/repository before
implementation. Do not point the coding agent at only the architecture document.

Read in order: AGENTS.md, ARCHITECTURE.md, DECISIONS.md, BUILD.md,
MILESTONES.md, COMPATIBILITY.md. Then use CLAUDE_PROMPT.md.

The architecture is a proposal. The first authorized implementation scope is M0
and M1 only. Status (2026-10-30): M0, M1 and M2 are implemented, plus an interim
console pulled forward from M4. The kernel boots under QEMU/UEFI, sets up its
own GDT/IDT, exception handlers, physical frame allocator, page mappings, a
1 MiB heap and a 100 Hz timer, runs self-tests, then shows a small debug shell
on the screen that you can type into (`help` lists its commands). Everything
else in ARCHITECTURE.md is unimplemented.

Quick start (after installing the prerequisites in tools/firmware/README.md):

    cargo xtask doctor
    cargo xtask verify
    cargo xtask run

OriginsOS is the current placeholder. Earlier alternatives were not checked
for naming conflicts. Use origins-os in technical identifiers for now.

Files:
- ARCHITECTURE.md: system diagrams and product direction.
- DECISIONS.md: initial decisions and explicitly unresolved choices.
- BUILD.md: reproducibility and command contract.
- MILESTONES.md: staged scope and acceptance criteria.
- AGENTS.md: instructions for implementing agents.
- COMPATIBILITY.md: application and infrastructure feasibility investigations.
- CLAUDE_PROMPT.md: ready-to-use kickoff prompt.
- kernel/, xtask/: M1 kernel and host build/run/verify tool.
- docs/m2-design.md: M2 memory, interrupt and timer design.
- docs/console-design.md: interim screen, keyboard and debug shell design.
- docs/verification.md: commands, versions and results actually observed.
- tools/firmware/README.md: QEMU and firmware setup and provenance.

