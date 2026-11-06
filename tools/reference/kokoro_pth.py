"""Reads Kokoro's PyTorch files (`kokoro-v1_0.pth`, `voices/*.pt`) as data
and writes their tensors as safetensors (docs/speech-design.md). Nothing in
ZooOS depends on it.

    python -I kokoro_pth.py <file.pth|file.pt> <out.safetensors> [--list]

A `.pth` is a zip: `data.pkl` (a pickle naming the tensors) and one raw
file per storage. The pickle is read by an unpickler that builds nothing
but tensors: `find_class` allows only the handful of names a state dict
uses, mapped to functions here, and refuses everything else, so no code
from the file can run (as `torch.load(weights_only=True)` does, without
torch).
"""

import json
import pickle
import struct
import sys
import zipfile
from collections import OrderedDict

import numpy as np

DTYPES = {
    "FloatStorage": ("<f4", "F32"),
    "HalfStorage": ("<f2", "F16"),
    "BFloat16Storage": (None, "BF16"),
    "LongStorage": ("<i8", "I64"),
    "IntStorage": ("<i4", "I32"),
}


class Storage:
    def __init__(self, kind, key):
        self.kind = kind
        self.key = key


def rebuild_tensor(storage, offset, size, stride, *_rest):
    return ("tensor", storage, offset, tuple(size), tuple(stride))


def rebuild_parameter(data, *_rest):
    return data


class StorageType:
    def __init__(self, name):
        self.name = name


ALLOWED = {
    ("torch._utils", "_rebuild_tensor_v2"): rebuild_tensor,
    ("torch._utils", "_rebuild_parameter"): rebuild_parameter,
    ("collections", "OrderedDict"): OrderedDict,
}


class DataOnly(pickle.Unpickler):
    def find_class(self, module, name):
        if (module, name) in ALLOWED:
            return ALLOWED[(module, name)]
        if module == "torch" and name in DTYPES:
            return StorageType(name)
        raise pickle.UnpicklingError(f"refused: {module}.{name}")

    def persistent_load(self, pid):
        # ('storage', storage_type, key, location, numel)
        if not (isinstance(pid, tuple) and pid[0] == "storage"):
            raise pickle.UnpicklingError(f"refused persistent id {pid!r}")
        return Storage(pid[1].name, pid[2])


def load(path):
    z = zipfile.ZipFile(path)
    names = z.namelist()
    pkl = next(n for n in names if n.endswith("data.pkl"))
    prefix = pkl[: -len("data.pkl")]
    obj = DataOnly(z.open(pkl)).load()
    out = OrderedDict()

    def walk(o, name):
        if isinstance(o, tuple) and o and o[0] == "tensor":
            _, st, offset, size, stride = o
            np_dtype, _ = DTYPES[st.kind]
            if np_dtype is None:
                raw = np.frombuffer(z.read(f"{prefix}data/{st.key}"), dtype="<u2")
                flat = (raw.astype(np.uint32) << 16).view(np.float32)
            else:
                flat = np.frombuffer(z.read(f"{prefix}data/{st.key}"), dtype=np_dtype)
            item = flat.itemsize
            a = np.lib.stride_tricks.as_strided(
                flat[offset:],
                shape=size,
                strides=tuple(s * item for s in stride),
            )
            out[name] = np.ascontiguousarray(a).astype(np.float32)
        elif isinstance(o, dict):
            for k, v in o.items():
                walk(v, f"{name}.{k}" if name else str(k))
        else:
            raise ValueError(f"{name}: unexpected {type(o).__name__}")

    walk(obj, "")
    return out


def save_safetensors(tensors, path):
    header = {}
    off = 0
    blobs = []
    for k, v in tensors.items():
        b = v.astype("<f4").tobytes()
        header[k] = {"dtype": "F32", "shape": list(v.shape), "data_offsets": [off, off + len(b)]}
        off += len(b)
        blobs.append(b)
    h = json.dumps(header).encode()
    h += b" " * ((8 - len(h) % 8) % 8)
    with open(path, "wb") as f:
        f.write(struct.pack("<Q", len(h)))
        f.write(h)
        for b in blobs:
            f.write(b)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    t = load(src)
    if "--list" in sys.argv:
        for k, v in t.items():
            print(k, list(v.shape))
    save_safetensors(t, dst)
    print(f"{len(t)} tensors, {sum(v.size for v in t.values())} values -> {dst}")


if __name__ == "__main__":
    main()
