# Start here

This is the implementation handoff for the ZooOS project (formerly the placeholder OriginsOS). It is not
Stonepyre. Copy this entire folder into its own new project/repository before
implementation. Do not point the coding agent at only the architecture document.

Read in order: AGENTS.md, ARCHITECTURE.md, DECISIONS.md, BUILD.md,
MILESTONES.md, COMPATIBILITY.md. Then use CLAUDE_PROMPT.md.

The architecture is a proposal. The first authorized implementation scope is M0
and M1 only. Status (2026-10-30): M0–M3 are implemented, plus an interim console pulled
forward from M4. The kernel boots under QEMU/UEFI, sets up its own CPU
tables, memory management and a preemptive scheduler, and runs isolated user
programs from a read-only initial filesystem: the Zookeeper supervisor and
its handlers talk over channels, survive a crashing handler and stop one that
never yields. Then a small debug shell appears on screen (`help` lists its
commands; `ps`, `ls`, `run`). Everything else in ARCHITECTURE.md is
unimplemented.

Quick start (after installing the prerequisites in tools/firmware/README.md):

    cargo xtask doctor
    cargo xtask verify
    cargo xtask run

Naming: the project was renamed from the placeholder OriginsOS to ZooOS on
2026-10-30. Technical identifiers use zoo-os. The runtime/service supervisor
planned for M3 will be called Zookeeper, and services animal-named handlers.
A web, GitHub and crates.io check found no OS or product called ZooOS; no
legal trademark search has been done. See DECISIONS.md.

Files:
- ARCHITECTURE.md: system diagrams and product direction.
- DECISIONS.md: initial decisions and explicitly unresolved choices.
- BUILD.md: reproducibility and command contract.
- MILESTONES.md: staged scope and acceptance criteria.
- AGENTS.md: instructions for implementing agents.
- COMPATIBILITY.md: application and infrastructure feasibility investigations.
- CLAUDE_PROMPT.md: ready-to-use kickoff prompt.
- kernel/, xtask/: the kernel and the host build/run/verify tool.
- abi/: system call ABI and the ELF and initrd parsers, shared by all.
- user/: the user runtime (`zoo_rt`) and programs (Zookeeper, handlers).
- docs/m2-design.md: M2 memory, interrupt and timer design.
- docs/console-design.md: interim screen, keyboard and debug shell design.
- docs/m3-design.md: processes, system calls, IPC and Zookeeper.
- docs/verification.md: commands, versions and results actually observed.
- tools/firmware/README.md: QEMU and firmware setup and provenance.

