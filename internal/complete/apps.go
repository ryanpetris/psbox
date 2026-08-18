package complete

// Application name completion via `psbox objects list applications --quiet`.

import (
	"os"
	"os/exec"
	"strings"

	"github.com/spf13/cobra"
)

// Applications lists configured application names for Cobra completion.
func Applications(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
	if len(args) > 0 {
		return nil, cobra.ShellCompDirectiveDefault
	}
	names, err := listApplications(cmd)
	if err != nil {
		return nil, cobra.ShellCompDirectiveError
	}
	return names, cobra.ShellCompDirectiveNoFileComp
}

// CommandArgs completes the trailing sandbox command the way a shell would.
func CommandArgs(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
	return nil, cobra.ShellCompDirectiveDefault
}

func listApplications(cmd *cobra.Command) ([]string, error) {
	exe, err := os.Executable()
	if err != nil {
		exe = "psbox"
	}
	argv := []string{"objects", "list", "applications", "--quiet"}
	if flag := cmd.Flags().Lookup("objects"); flag != nil && flag.Changed {
		argv = []string{"objects", "--objects", flag.Value.String(), "list", "applications", "--quiet"}
	}
	out, err := exec.Command(exe, argv...).Output()
	if err != nil {
		return nil, err
	}
	var names []string
	for _, line := range strings.Split(string(out), "\n") {
		line = strings.TrimSpace(line)
		if line != "" {
			names = append(names, line)
		}
	}
	return names, nil
}
