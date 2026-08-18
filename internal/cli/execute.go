package cli

// Process entry for the Cobra command tree.

import (
	"errors"
	"fmt"
	"os"

	"github.com/spf13/cobra"

	"petris.dev/psbox/internal/instance"
)

// Execute runs cmd and returns a process exit code.
func Execute(cmd *cobra.Command) int {
	if len(os.Args) == 1 {
		_ = cmd.Help()
		return 2
	}
	cmd.SilenceErrors = true
	if err := cmd.Execute(); err != nil {
		if errors.Is(err, errHelp) {
			return 2
		}
		var st *instance.StatusError
		if errors.As(err, &st) {
			return st.Code
		}
		fmt.Fprintln(cmd.ErrOrStderr(), err)
		return 1
	}
	return 0
}
