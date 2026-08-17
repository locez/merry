# Merry Roadmap

Detailed scope, acceptance, evidence, and status live in the linked GitHub
issues. This file keeps only the major delivery sequence and its dependencies.

## Current Focus

- [x] [T0: Establish delivery baseline, roadmap, and architecture guard](https://github.com/locez/merry/issues/10)

## Initiative

- [ ] [Parent: Establish an evaluable, reusable, production-grade Rust coding agent boundary](https://github.com/locez/merry/issues/9)
- [x] [T0: Establish delivery baseline, roadmap, and architecture guard](https://github.com/locez/merry/issues/10)
- [x] [T1: Establish evaluation protocol and TaskSpec](https://github.com/locez/merry/issues/8)
- [ ] [T2: Establish deterministic Offline Harness](https://github.com/locez/merry/issues/11)
- [ ] [T3: Establish the unique CodingAgentProfile](https://github.com/locez/merry/issues/12)
- [ ] [T4: Establish the internal Coding Eval Suite](https://github.com/locez/merry/issues/13)
- [ ] [T5: Unify CLI, Debug, and Rust Runtime Builder](https://github.com/locez/merry/issues/14)
- [ ] [T6: Establish production reliability, security, and observability harness](https://github.com/locez/merry/issues/15)
- [ ] [T7: Release a stable Rust SDK](https://github.com/locez/merry/issues/16)
- [ ] [T8: Align Python and Rust SDK capabilities](https://github.com/locez/merry/issues/17)
- [ ] [T9: Integrate external coding-agent evaluation suites](https://github.com/locez/merry/issues/18)
- [ ] [T10: Split large Runtime and Binding modules by responsibility](https://github.com/locez/merry/issues/19)
- [ ] [T11: Establish release gates and close the Definition of Done](https://github.com/locez/merry/issues/20)

## Dependency Order

```text
T0 -> T1 -> T2
T0 -> T3
T2 + T3 -> T4
T3 + T4 -> T5
T2 + T4 + T5 -> T6
T5 + T6 -> T7 -> T8
T1 + T2 + T4 -> T9
T4 + T5 + T7 + T8 -> T10
T6 + T9 + T10 -> T11
```

The architecture ownership and dependency contract is maintained in
`AGENTS.md`; issue descriptions remain the source of truth for implementation
details and progress.
