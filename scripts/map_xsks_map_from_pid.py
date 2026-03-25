#!/usr/bin/env python3
"""Map an AF_XDP socket fd from a target PID into a pinned XSKMAP entry.

This avoids manual one-off Python snippets when running the AF_XDP example.
"""

import argparse
import ctypes as C
import os
import sys

# x86_64 syscall numbers
SYS_BPF = 321
SYS_PIDFD_OPEN = 434
SYS_PIDFD_GETFD = 438

BPF_MAP_UPDATE_ELEM = 2
BPF_OBJ_GET = 7


class BpfObjGetAttr(C.Structure):
    _fields_ = [
        ("pathname", C.c_uint64),
        ("bpf_fd", C.c_uint32),
        ("file_flags", C.c_uint32),
        ("path_fd", C.c_int32),
        ("_pad", C.c_uint32),
    ]


class BpfMapUpdateElemAttr(C.Structure):
    _fields_ = [
        ("map_fd", C.c_uint32),
        ("_pad0", C.c_uint32),
        ("key", C.c_uint64),
        ("value", C.c_uint64),
        ("flags", C.c_uint64),
    ]


def sys_call(libc, num, *args):
    result = libc.syscall(num, *args)
    if result < 0:
        err = C.get_errno()
        raise OSError(err, os.strerror(err))
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid", type=int, required=True, help="Target process PID")
    parser.add_argument(
        "--map-path",
        default="/sys/fs/bpf/xsks_map",
        help="Pinned XSKMAP path",
    )
    parser.add_argument("--queue-id", type=int, default=0, help="XSKMAP queue key")
    parser.add_argument(
        "--fd-start", type=int, default=3, help="Start fd scan at this value"
    )
    parser.add_argument(
        "--fd-end", type=int, default=1024, help="End fd scan (exclusive)"
    )
    args = parser.parse_args()

    libc = C.CDLL(None, use_errno=True)
    libc.syscall.restype = C.c_long

    map_path = C.create_string_buffer(args.map_path.encode("utf-8"))
    obj_attr = BpfObjGetAttr(C.addressof(map_path), 0, 0, 0, 0)
    map_fd = sys_call(libc, SYS_BPF, BPF_OBJ_GET, C.byref(obj_attr), C.sizeof(obj_attr))

    pidfd = sys_call(libc, SYS_PIDFD_OPEN, args.pid, 0)

    key = C.c_uint32(args.queue_id)
    ok = False
    mapped_target_fd = None
    mapped_dup_fd = None

    try:
        for target_fd in range(args.fd_start, args.fd_end):
            try:
                dup_fd = sys_call(libc, SYS_PIDFD_GETFD, pidfd, target_fd, 0)
            except OSError:
                continue

            try:
                value = C.c_uint32(dup_fd)
                update_attr = BpfMapUpdateElemAttr(
                    map_fd,
                    0,
                    C.addressof(key),
                    C.addressof(value),
                    0,
                )
                ret = libc.syscall(
                    SYS_BPF,
                    BPF_MAP_UPDATE_ELEM,
                    C.byref(update_attr),
                    C.sizeof(update_attr),
                )
                if ret == 0:
                    ok = True
                    mapped_target_fd = target_fd
                    mapped_dup_fd = dup_fd
                    break
            finally:
                os.close(dup_fd)
    finally:
        os.close(pidfd)
        os.close(map_fd)

    if not ok:
        print(
            f"error: failed to map queue {args.queue_id} from pid {args.pid} into {args.map_path}",
            file=sys.stderr,
        )
        return 1

    print(
        f"mapped xsks_map[{args.queue_id}] from pid={args.pid} target_fd={mapped_target_fd} dup_fd={mapped_dup_fd}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
