package main

// Command psbox is the host CLI.

import (
	"os"

	"petris.dev/psbox/internal/app/client"
)

func main() {
	os.Exit(client.Run())
}
