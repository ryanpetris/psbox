package instance

// SEQPACKET JSON frames with optional SCM_RIGHTS fds.

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"sort"

	"golang.org/x/sys/unix"
)

// WriteMsg writes one SEQPACKET frame and optional file descriptors.
func WriteMsg(c *net.UnixConn, msg Message, files []*os.File) error {
	payload, err := json.Marshal(msg)
	if err != nil {
		return err
	}
	if len(payload) > MaxMessageBytes {
		return fmt.Errorf("%s", oversizedError(msg.Env, len(payload)))
	}
	var oob []byte
	if len(files) > 0 {
		fds := make([]int, len(files))
		for i, f := range files {
			fds[i] = int(f.Fd())
		}
		oob = unix.UnixRights(fds...)
	}
	_, _, err = c.WriteMsgUnix(payload, oob, nil)
	return err
}

// ReadMsg reads one SEQPACKET frame and up to maxFDs file descriptors.
func ReadMsg(c *net.UnixConn, maxFDs int) (Message, []*os.File, error) {
	buf := make([]byte, MaxMessageBytes)
	oob := make([]byte, unix.CmsgSpace(4*maxFDs))
	n, oobn, _, _, err := c.ReadMsgUnix(buf, oob)
	if err != nil {
		return Message{}, nil, err
	}
	if n == MaxMessageBytes {
		return Message{}, nil, fmt.Errorf("protocol message exceeds %d bytes", MaxMessageBytes)
	}
	var msg Message
	if err := json.Unmarshal(buf[:n], &msg); err != nil {
		return Message{}, nil, fmt.Errorf("decode protocol message: %w", err)
	}
	if oobn == 0 || maxFDs == 0 {
		return msg, nil, nil
	}
	scms, err := unix.ParseSocketControlMessage(oob[:oobn])
	if err != nil {
		return Message{}, nil, err
	}
	var files []*os.File
	for _, scm := range scms {
		fds, err := unix.ParseUnixRights(&scm)
		if err != nil {
			closeFiles(files)
			return Message{}, nil, err
		}
		for _, fd := range fds {
			files = append(files, os.NewFile(uintptr(fd), "passed"))
		}
	}
	return msg, files, nil
}

func closeFiles(files []*os.File) {
	for _, f := range files {
		if f != nil {
			_ = f.Close()
		}
	}
}

func oversizedError(env map[string]string, n int) string {
	type kv struct {
		name string
		size int
	}
	var list []kv
	for k, v := range env {
		list = append(list, kv{k, len(k) + len(v)})
	}
	sort.Slice(list, func(i, j int) bool { return list[i].size > list[j].size })
	msg := fmt.Sprintf("spawn environment exceeds %d bytes (%d encoded)", MaxMessageBytes, n)
	if len(list) == 0 {
		return msg
	}
	limit := 3
	if len(list) < limit {
		limit = len(list)
	}
	msg += "; largest variables:"
	for i := 0; i < limit; i++ {
		msg += fmt.Sprintf(" %s (%d)", list[i].name, list[i].size)
	}
	return msg
}
