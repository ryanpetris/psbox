package main

// Command psboxd is the host instance daemon.

import (
	"os"

	"petris.dev/psbox/internal/app/daemon"
)

func main() {
	os.Exit(daemon.Run())
}
