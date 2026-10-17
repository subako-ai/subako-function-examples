//go:build wasip1

package main

import (
	"syscall"
	"time"
	_ "unsafe" // for go:linkname

	"github.com/go-git/go-git/v6/plumbing/format/index"
)

// fillSystemInfo is go-git's hook for the stat fields of an index entry.
// go-git's own wasip1 hook reads Stat_t.Ctime as seconds, but WASI counts
// nanoseconds, so the entry's time overflows and writing the index fails.
//
//go:linkname fillSystemInfo github.com/go-git/go-git/v6.fillSystemInfo
var fillSystemInfo func(e *index.Entry, sys any)

// go-git's package init has run before this one, so this replaces its hook.
func init() {
	fillSystemInfo = func(e *index.Entry, sys any) {
		if st, ok := sys.(*syscall.Stat_t); ok {
			e.CreatedAt = time.Unix(0, int64(st.Ctime))
			e.Dev = uint32(st.Dev)
			e.Inode = uint32(st.Ino)
			e.GID = st.Gid
			e.UID = st.Uid
		}
	}
}
