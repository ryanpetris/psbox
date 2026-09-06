package main

// Command psboxa is the in-sandbox agent and the xdg-open helper.

import (
	"os"

	"petris.dev/psbox/internal/app/agent"
)

func main() {
	os.Exit(agent.Run())
}
