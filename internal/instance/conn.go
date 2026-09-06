package instance

// SEQPACKET JSON frames with optional SCM_RIGHTS fds.

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"sort"
	"time"

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
	// A timeout invalidates the stream. Its independent timer also bounds time
	// spent queued behind another writer; concurrent writes cannot extend it.
	expired := make(chan struct{})
	timer := time.AfterFunc(FirstReplyTimeout, func() { _ = c.Close(); close(expired) })
	_, _, err = c.WriteMsgUnix(payload, oob, nil)
	if !timer.Stop() {
		<-expired
		return fmt.Errorf("write protocol message: %w", os.ErrDeadlineExceeded)
	}
	return err
}

// ReadMsg reads one SEQPACKET frame and up to maxFDs file descriptors.
func ReadMsg(c *net.UnixConn, maxFDs int) (Message, []*os.File, error) {
	if maxFDs < 0 {
		return Message{}, nil, fmt.Errorf("negative descriptor limit")
	}
	buf := make([]byte, MaxMessageBytes)
	// Receive the Linux SCM_RIGHTS maximum so every installed descriptor is owned,
	// including descriptors exceeding the message-specific limit.
	oob := make([]byte, unix.CmsgSpace(4*253))
	n, oobn, flags, _, readErr := c.ReadMsgUnix(buf, oob)
	var files []*os.File
	accepted := false
	defer func() {
		if !accepted {
			closeFiles(files)
		}
	}()
	scms, err := unix.ParseSocketControlMessage(oob[:oobn])
	if err != nil {
		return Message{}, nil, fmt.Errorf("parse control message: %w", err)
	}
	var controlErr error
	for _, scm := range scms {
		if scm.Header.Level != unix.SOL_SOCKET || scm.Header.Type != unix.SCM_RIGHTS {
			controlErr = fmt.Errorf("unexpected ancillary message")
			continue
		}
		fds, err := unix.ParseUnixRights(&scm)
		if err != nil {
			controlErr = err
			continue
		}
		for _, fd := range fds {
			unix.CloseOnExec(fd)
			files = append(files, os.NewFile(uintptr(fd), "passed"))
		}
	}
	if readErr != nil {
		return Message{}, nil, readErr
	}
	if controlErr != nil {
		return Message{}, nil, controlErr
	}
	if flags&(unix.MSG_TRUNC|unix.MSG_CTRUNC) != 0 {
		return Message{}, nil, fmt.Errorf("truncated protocol message")
	}
	if len(files) > maxFDs {
		return Message{}, nil, fmt.Errorf("protocol message has %d descriptors; maximum is %d", len(files), maxFDs)
	}
	var msg Message
	if err := json.Unmarshal(buf[:n], &msg); err != nil {
		return Message{}, nil, fmt.Errorf("decode protocol message: %w", err)
	}
	accepted = true
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
