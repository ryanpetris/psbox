package main

// Command psboxa is the in-sandbox agent and the xdg-open helper.

import (
	"os"

	"petris.dev/psbox/internal/app/agent"
	"petris.dev/psbox/internal/instance"
)

func main() {
	if instance.InvokedAsXDGOpen(os.Args) {
		os.Exit(instance.RunXDGOpen(os.Args))
	}
	os.Exit(agent.Run())
}
