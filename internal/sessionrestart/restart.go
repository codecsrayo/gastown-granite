// Package sessionrestart builds the shell command used by tmux respawn-pane
// when restarting a Gas Town agent session.
//
// This is the shared form of buildRestartCommandWithOpts, lifted out of
// internal/cmd so daemon-side code (quota rotation, watchdogs) can rebuild
// the same command without shelling back into the gt binary. The cmd path
// keeps thin wrappers around this package for backwards source compat.
package sessionrestart

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/steveyegge/gastown/internal/cli"
	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/mayor"
	"github.com/steveyegge/gastown/internal/session"
	"github.com/steveyegge/gastown/internal/tmux"
	"github.com/steveyegge/gastown/internal/workspace"
)

// ClaudeEnvVars lists Claude-related environment variables to propagate
// during handoff. These vars aren't inherited by tmux respawn-pane's fresh
// shell. Exported so callers can extend it for custom telemetry setups.
var ClaudeEnvVars = []string{
	"ANTHROPIC_API_KEY",
	"CLAUDE_CODE_USE_BEDROCK",
	"AWS_PROFILE",
	"AWS_REGION",
	"CLAUDE_CODE_ENABLE_TELEMETRY",
	"OTEL_METRICS_EXPORTER",
	"OTEL_METRIC_EXPORT_INTERVAL",
	"OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
	"OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
	"OTEL_LOGS_EXPORTER",
	"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
	"OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
	"OTEL_LOG_TOOL_DETAILS",
	"OTEL_LOG_TOOL_CONTENT",
	"OTEL_LOG_USER_PROMPTS",
	"OTEL_RESOURCE_ATTRIBUTES",
	"BD_OTEL_METRICS_URL",
	"BD_OTEL_LOGS_URL",
	"GT_OTEL_METRICS_URL",
	"GT_OTEL_LOGS_URL",
}

// Options controls restart command generation.
type Options struct {
	// ContinueSession adds --continue and omits the beacon prompt,
	// so the agent resumes its previous conversation silently.
	ContinueSession bool

	// ContinuePrompt overrides the default continuation prompt when
	// ContinueSession is true. Empty falls back to a generic message.
	ContinuePrompt string
}

// BuildCommand creates the shell command tmux runs in a respawned pane.
// The returned string assumes a POSIX shell on non-Windows and PowerShell
// on Windows. Caller passes the result to tmux.RespawnPane.
func BuildCommand(sessionName string, opts Options) (string, error) {
	townRoot := DetectTownRoot()
	if townRoot == "" {
		return "", fmt.Errorf("cannot detect town root - run from within a Gas Town workspace")
	}

	workDir, err := SessionWorkDir(sessionName, townRoot)
	if err != nil {
		return "", err
	}

	identity, err := session.ParseSessionName(sessionName)
	if err != nil {
		return "", fmt.Errorf("cannot parse session name %q: %w", sessionName, err)
	}
	gtRole := identity.GTRole()
	simpleRole := config.ExtractSimpleRole(gtRole)

	rigPath := ""
	if identity.Rig != "" {
		rigPath = filepath.Join(townRoot, identity.Rig)
	}

	beacon := buildBeacon(opts, identity, simpleRole)
	currentAgent := resolveCurrentAgent(sessionName)

	runtimeCmd, err := resolveRuntimeCommand(currentAgent, simpleRole, rigPath, townRoot, beacon)
	if err != nil {
		return "", err
	}
	if opts.ContinueSession {
		runtimeCmd = injectContinueFlag(runtimeCmd)
	}

	envMap, agentEnv := buildEnvMap(currentAgent, simpleRole, gtRole, rigPath, townRoot)
	mergeAgentEnv(envMap, agentEnv)
	zeroNodeOptionsIfAbsent(envMap, agentEnv)

	return prefixCommand(workDir, runtimeCmd, envMap), nil
}

// SessionWorkDir returns the canonical working directory for a session.
func SessionWorkDir(sessionName, townRoot string) (string, error) {
	mayorSession := mayor.SessionName()
	deaconSession := session.DeaconSessionName()
	bootSession := session.BootSessionName()

	switch {
	case sessionName == mayorSession:
		return townRoot + "/mayor", nil
	case sessionName == bootSession:
		return townRoot + "/deacon/dogs/boot", nil
	case sessionName == deaconSession:
		return townRoot + "/deacon", nil
	case strings.Contains(sessionName, "-crew-"):
		rig, name, ok := parseCrew(sessionName)
		if !ok {
			return "", fmt.Errorf("cannot parse crew session name: %s", sessionName)
		}
		return fmt.Sprintf("%s/%s/crew/%s", townRoot, rig, name), nil
	}

	identity, err := session.ParseSessionName(sessionName)
	if err != nil {
		return "", fmt.Errorf("unknown session type: %s (%w)", sessionName, err)
	}
	switch identity.Role {
	case session.RoleMayor:
		return townRoot + "/mayor", nil
	case session.RoleDeacon, session.RoleOverseer:
		return townRoot + "/deacon", nil
	case session.RoleWitness:
		return fmt.Sprintf("%s/%s/witness", townRoot, identity.Rig), nil
	case session.RoleRefinery:
		return fmt.Sprintf("%s/%s/refinery/rig", townRoot, identity.Rig), nil
	case session.RolePolecat:
		return fmt.Sprintf("%s/%s/polecats/%s", townRoot, identity.Rig, identity.Name), nil
	case session.RoleDog:
		return fmt.Sprintf("%s/deacon/dogs/%s", townRoot, identity.Name), nil
	default:
		return "", fmt.Errorf("unknown session type: %s (role %s, try specifying role explicitly)", sessionName, identity.Role)
	}
}

// DetectTownRoot walks up from the current directory to find the town root,
// falling back to GT_TOWN_ROOT / GT_ROOT env vars and finally the tmux
// global environment. Returns "" when nothing identifies a workspace.
func DetectTownRoot() string {
	if townRoot, err := workspace.FindFromCwd(); err == nil && townRoot != "" {
		return townRoot
	}
	for _, envName := range []string{"GT_TOWN_ROOT", "GT_ROOT"} {
		if envRoot := os.Getenv(envName); envRoot != "" {
			if looksLikeWorkspace(envRoot) {
				return envRoot
			}
		}
	}
	if socket := tmux.SocketFromEnv(); socket != "" {
		t := tmux.NewTmuxWithSocket(socket)
		if envRoot, err := t.GetGlobalEnvironment("GT_TOWN_ROOT"); err == nil && envRoot != "" {
			if looksLikeWorkspace(envRoot) {
				return envRoot
			}
		}
	}
	return ""
}

// IsPatrolRole reports whether a role re-enters a patrol loop on restart
// rather than waiting for new instructions.
func IsPatrolRole(role string) bool {
	switch role {
	case "refinery", "witness", "deacon":
		return true
	}
	return false
}

// --- internal helpers -------------------------------------------------------

func looksLikeWorkspace(root string) bool {
	if _, err := os.Stat(filepath.Join(root, workspace.PrimaryMarker)); err == nil {
		return true
	}
	info, err := os.Stat(filepath.Join(root, workspace.SecondaryMarker))
	return err == nil && info.IsDir()
}

func parseCrew(sessionName string) (rig, name string, ok bool) {
	identity, err := session.ParseSessionName(sessionName)
	if err != nil {
		return "", "", false
	}
	if identity.Role != session.RoleCrew || identity.Rig == "" || identity.Name == "" {
		return "", "", false
	}
	return identity.Rig, identity.Name, true
}

func buildBeacon(opts Options, identity *session.AgentIdentity, simpleRole string) string {
	if opts.ContinueSession {
		if opts.ContinuePrompt != "" {
			return opts.ContinuePrompt
		}
		return "Your account was rotated to avoid a rate limit. Continue your previous task."
	}
	if IsPatrolRole(simpleRole) {
		return session.BuildStartupPrompt(session.BeaconConfig{
			Recipient: identity.BeaconAddress(),
			Sender:    "self",
			Topic:     "patrol",
		}, "Run `"+cli.Name()+" prime --hook` and begin patrol.")
	}
	return session.FormatStartupBeacon(session.BeaconConfig{
		Recipient: identity.BeaconAddress(),
		Sender:    "self",
		Topic:     "handoff",
	})
}

func resolveCurrentAgent(sessionName string) string {
	agent, ok := os.LookupEnv("GT_AGENT")
	if ok {
		return agent
	}
	t := tmux.NewTmux()
	if val, err := t.GetEnvironment(sessionName, "GT_AGENT"); err == nil && val != "" {
		return val
	}
	return ""
}

func resolveRuntimeCommand(currentAgent, simpleRole, rigPath, townRoot, beacon string) (string, error) {
	if currentAgent != "" {
		cmd, err := config.GetRuntimeCommandWithPromptAndAgentOverride(rigPath, beacon, currentAgent)
		if err != nil {
			return "", fmt.Errorf("resolving agent config: %w", err)
		}
		return cmd, nil
	}
	if simpleRole != "" {
		return config.ResolveRoleAgentConfig(simpleRole, townRoot, rigPath).BuildCommandWithPrompt(beacon), nil
	}
	return config.GetRuntimeCommandWithPrompt(rigPath, beacon), nil
}

func injectContinueFlag(runtimeCmd string) string {
	if n := strings.Replace(runtimeCmd, "claude.exe ", "claude.exe --continue ", 1); n != runtimeCmd {
		return n
	}
	return strings.Replace(runtimeCmd, "claude ", "claude --continue ", 1)
}

func buildEnvMap(currentAgent, simpleRole, gtRole, rigPath, townRoot string) (map[string]string, map[string]string) {
	envMap := make(map[string]string)
	var agentEnv map[string]string

	if gtRole != "" {
		var rc *config.RuntimeConfig
		switch {
		case currentAgent != "":
			if resolved, _, err := config.ResolveAgentConfigWithOverride(townRoot, rigPath, currentAgent); err == nil {
				rc = resolved
			} else {
				rc = config.ResolveRoleAgentConfig(simpleRole, townRoot, rigPath)
			}
		case simpleRole != "":
			rc = config.ResolveRoleAgentConfig(simpleRole, townRoot, rigPath)
		default:
			rc = config.ResolveAgentConfig(townRoot, rigPath)
		}
		agentEnv = rc.Env
		envMap["GT_ROLE"] = gtRole
		envMap["BD_ACTOR"] = gtRole
		envMap["GIT_AUTHOR_NAME"] = gtRole
		if rc.Session != nil && rc.Session.SessionIDEnv != "" {
			envMap["GT_SESSION_ID_ENV"] = rc.Session.SessionIDEnv
		}
	}

	envMap["GT_ROOT"] = townRoot
	if currentAgent != "" {
		envMap["GT_AGENT"] = currentAgent
	}

	if processNames := os.Getenv("GT_PROCESS_NAMES"); processNames != "" {
		envMap["GT_PROCESS_NAMES"] = processNames
	} else if currentAgent != "" {
		resolved := config.ResolveProcessNames(currentAgent, "")
		envMap["GT_PROCESS_NAMES"] = strings.Join(resolved, ",")
	}

	for _, name := range ClaudeEnvVars {
		if val := os.Getenv(name); val != "" {
			envMap[name] = val
		}
	}
	return envMap, agentEnv
}

func mergeAgentEnv(envMap, agentEnv map[string]string) {
	for k, v := range agentEnv {
		if _, exists := envMap[k]; !exists {
			envMap[k] = v
		}
	}
}

func zeroNodeOptionsIfAbsent(envMap, agentEnv map[string]string) {
	if _, hasNodeOpts := agentEnv["NODE_OPTIONS"]; !hasNodeOpts {
		envMap["NODE_OPTIONS"] = ""
	}
}

func prefixCommand(workDir, runtimeCmd string, envMap map[string]string) string {
	var cdPrefix, execPrefix string
	if runtime.GOOS == "windows" {
		cdPrefix = fmt.Sprintf("cd %s; ", workDir)
	} else {
		cdPrefix = fmt.Sprintf("cd %s && ", workDir)
		execPrefix = "exec "
	}
	envCmd := config.PrependEnv(execPrefix+runtimeCmd, envMap)
	return cdPrefix + envCmd
}
