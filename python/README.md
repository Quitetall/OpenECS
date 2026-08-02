# oheeg-val

Python launcher and typed subprocess API for OpenECS. OpenECS remains sole
grading implementation; this wheel does not duplicate metric or tier logic.

Install `openecs` first, then use either interface:

```bash
oheeg-val verify-corpus --corpus-manifest corpus.toml
```

```python
from oheeg_val import run_openecs

result = run_openecs(["verify-corpus", "--corpus-manifest", "corpus.toml"])
result.check_returncode()
```

Exit status matches `openecs`. Missing executable returns 127 from console
launcher and raises `FileNotFoundError` from Python API.

