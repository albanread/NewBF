# beef-tests

A curated corpus of Beef sample programs used as NewBF's faithful-tribute
regression battery (`tests/newbf-compat-tests`). Each sample is plain Beef
that should compile and run identically under upstream Beef and NewBF.

- `samples/` — small, hermetic programs, grown sprint by sprint as
  language features land.

Larger fixtures will be curated from `E:\beef\BeefLibs\corlib\src\` and the
upstream `IDEHelper\Tests\` tree as the corlib port advances. The upstream
tree at `E:\beef` is read-only reference — we copy fixtures here rather
than reaching into it.
