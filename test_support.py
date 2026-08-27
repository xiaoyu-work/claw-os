import importlib.util
import sys
from pathlib import Path


def load_local_module(path, name, *, clear_modules=()):
    path = Path(path)
    apps_root = Path(__file__).resolve().parent / "apps"
    if str(apps_root) not in sys.path:
        sys.path.insert(0, str(apps_root))
    for prefix in clear_modules:
        for module_name in list(sys.modules):
            if module_name == prefix or module_name.startswith(f"{prefix}."):
                sys.modules.pop(module_name, None)
    sys.modules.pop(name, None)
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load module from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except Exception:
        sys.modules.pop(name, None)
        raise
    return module
