# Comptime

`newbf-comptime` is a genuine compile-time interpreter — the one part of
the system that is not the JIT — modelling Beef's `CeMachine`
(`E:\beef\IDEHelper\Compiler\CeMachine.cpp`). It runs `[Comptime]`
methods, const-evaluates expressions, and generates types/members during
compilation.

It is invoked re-entrantly from `newbf-sema`: comptime can produce types
that feed back into resolution. To avoid a circular crate dependency, sema
calls into comptime through a trait-object callback. A comptime evaluation
trace is one of the human-reviewable phase reports.

Stub — expanded at SPRINTS.md Sprints 19–21.
