package instance

// Received descriptors are owned and closed even when a frame is rejected.

import (
	"bytes"
	"errors"
	"io"
	"net"
	"os"
	"sync"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

func TestReadMsgRejectsFrameAndClosesDescriptors(t *testing.T) {
	for _, tc := range []struct {
		name         string
		payload      []byte
		limit, count int
	}{
		{"invalid JSON", []byte("{"), 1, 1},
		{"forbidden descriptors", []byte(`{"type":"signal"}`), 0, 1},
		{"excess descriptors", []byte(`{"type":"spawn"}`), 1, 3},
		{"truncated payload", bytes.Repeat([]byte(" "), MaxMessageBytes+1), 1, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			sender, receiver := seqpacketPair(t)
			defer sender.Close()
			defer receiver.Close()
			raw, err := sender.SyscallConn()
			if err != nil {
				t.Fatal(err)
			}
			var socketErr error
			if err := raw.Control(func(fd uintptr) {
				socketErr = unix.SetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_SNDBUF, 2*MaxMessageBytes)
			}); err != nil {
				t.Fatal(err)
			}
			if socketErr != nil {
				t.Fatal(socketErr)
			}
			r, w, err := os.Pipe()
			if err != nil {
				t.Fatal(err)
			}
			defer r.Close()
			defer w.Close()
			fds := make([]int, tc.count)
			for i := range fds {
				fds[i] = int(w.Fd())
			}
			_, _, err = sender.WriteMsgUnix(tc.payload, unix.UnixRights(fds...), nil)
			if errors.Is(err, unix.EMSGSIZE) && len(tc.payload) > MaxMessageBytes {
				t.Skip("kernel limits SEQPACKET below protocol size")
			}
			if err != nil {
				t.Fatal(err)
			}
			if err := w.Close(); err != nil {
				t.Fatal(err)
			}
			_, files, err := ReadMsg(receiver, tc.limit)
			if err == nil {
				closeFiles(files)
				t.Fatal("accepted invalid frame")
			}
			if len(files) != 0 {
				t.Fatal("returned descriptors on error")
			}
			if err := r.SetReadDeadline(time.Now().Add(time.Second)); err != nil {
				t.Fatal(err)
			}
			if _, err := r.Read(make([]byte, 1)); !errors.Is(err, io.EOF) {
				t.Fatalf("received write end leaked: %v", err)
			}
		})
	}
}

func TestReadMsgDescriptorUsableAndCloseOnExec(t *testing.T) {
	sender, receiver := seqpacketPair(t)
	defer sender.Close()
	defer receiver.Close()
	f, err := os.Open(os.DevNull)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if err := WriteMsg(sender, Message{Type: TypeSpawn}, []*os.File{f}); err != nil {
		t.Fatal(err)
	}
	_, files, err := ReadMsg(receiver, 1)
	if err != nil {
		t.Fatal(err)
	}
	defer closeFiles(files)
	if len(files) != 1 {
		t.Fatalf("received %d files", len(files))
	}
	flags, err := unix.FcntlInt(files[0].Fd(), unix.F_GETFD, 0)
	if err != nil || flags&unix.FD_CLOEXEC == 0 {
		t.Fatalf("flags=%d: %v", flags, err)
	}
	if _, err := files[0].Stat(); err != nil {
		t.Fatal(err)
	}
}

func TestWriteMsgTimeoutCannotBeExtendedByOtherWriters(t *testing.T) {
	sender, peer := seqpacketPair(t)
	defer peer.Close()
	defer sender.Close()
	raw, err := sender.SyscallConn()
	if err != nil {
		t.Fatal(err)
	}
	var fillErr error
	if err := raw.Control(func(fd uintptr) {
		for {
			fillErr = unix.Send(int(fd), []byte(`{"type":"signal"}`), unix.MSG_DONTWAIT)
			if fillErr != nil {
				return
			}
		}
	}); err != nil {
		t.Fatal(err)
	}
	if !errors.Is(fillErr, unix.EAGAIN) {
		t.Fatalf("fill socket: %v", fillErr)
	}
	var writers sync.WaitGroup
	first := make(chan error, 1)
	writers.Go(func() { first <- WriteMsg(sender, Message{Type: TypeSignal}, nil) })
	defer func() { _ = sender.Close(); writers.Wait() }()
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	timeout := time.NewTimer(FirstReplyTimeout + time.Second)
	defer timeout.Stop()
	for {
		select {
		case err := <-first:
			if !errors.Is(err, os.ErrDeadlineExceeded) {
				t.Fatalf("first write: %v", err)
			}
			if _, _, err := ReadMsg(sender, 0); !errors.Is(err, net.ErrClosed) {
				t.Fatalf("timed-out connection stayed open: %v", err)
			}
			return
		case <-ticker.C:
			writers.Go(func() { _ = WriteMsg(sender, Message{Type: TypeSignal}, nil) })
		case <-timeout.C:
			t.Fatal("concurrent writers extended the first write deadline")
		}
	}
}
