from __future__ import annotations
import ctypes
from rustfst.utils import (
    lib,
    check_ffi_error,
)

from rustfst.fst.vector_fst import VectorFst


def reverse(fst: VectorFst):
    """
    reverse(fst)
    reverse an fst
    :param fst: Fst
    :return: Fst
    """
    ret_code = lib.fst_reverse(ctypes.byref(fst.ptr))
    err_msg = "Error during reverse"
    check_ffi_error(ret_code, err_msg)

    return fst
