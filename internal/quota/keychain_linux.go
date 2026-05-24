//go:build linux

package quota

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// On Linux, Claude Code stores its OAuth credentials as a plain JSON file at
// <configDir>/.credentials.json. There is no system keyring involvement, so
// "keychain" operations are implemented as file-backed read/write/copy. The
// rest of the rotation flow (oauthAccount swap, restart, etc.) is unchanged.

const credentialsFileName = ".credentials.json"

// KeychainCredential holds a backup of a credential blob for rollback.
type KeychainCredential struct {
	ServiceName string // resolved config dir path (canonical "service" identifier)
	Token       string // backed-up raw credentials.json content
}

func credentialsPath(configDir string) string {
	return filepath.Join(expandTilde(configDir), credentialsFileName)
}

// KeychainServiceName returns the canonical config dir path that identifies
// the credential store on Linux. Used as the ServiceName in KeychainCredential
// backups so restore can locate the right file.
func KeychainServiceName(configDir string) string {
	return expandTilde(configDir)
}

// ReadKeychainToken returns the raw .credentials.json content for a config dir.
func ReadKeychainToken(configDir string) (string, error) {
	path := credentialsPath(configDir)
	data, err := os.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("reading credentials at %s: %w", path, err)
	}
	return string(data), nil
}

// WriteKeychainToken writes the raw credential blob to <configDir>/.credentials.json.
// The accountLabel parameter is ignored on Linux (the file path is the identifier).
func WriteKeychainToken(configDir, _, token string) error {
	path := credentialsPath(configDir)
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("ensuring config dir %s: %w", filepath.Dir(path), err)
	}
	if err := os.WriteFile(path, []byte(token), 0600); err != nil {
		return fmt.Errorf("writing credentials at %s: %w", path, err)
	}
	return nil
}

// SwapKeychainCredential backs up the target's credentials, then overwrites
// them with the source's credentials. Returns the backup for rollback.
//
// The target config dir is preserved so the spawned session sees the same
// transcript on /resume; only the embedded auth token changes.
func SwapKeychainCredential(targetConfigDir, sourceConfigDir string) (*KeychainCredential, error) {
	backup, err := ReadKeychainToken(targetConfigDir)
	if err != nil {
		return nil, fmt.Errorf("backing up target token: %w", err)
	}
	source, err := ReadKeychainToken(sourceConfigDir)
	if err != nil {
		return nil, fmt.Errorf("reading source token: %w", err)
	}
	if err := WriteKeychainToken(targetConfigDir, "claude-code", source); err != nil {
		return nil, fmt.Errorf("writing source token to target: %w", err)
	}
	return &KeychainCredential{
		ServiceName: KeychainServiceName(targetConfigDir),
		Token:       backup,
	}, nil
}

// RestoreKeychainToken writes the backup credentials back, undoing a prior
// SwapKeychainCredential.
func RestoreKeychainToken(backup *KeychainCredential) error {
	if backup == nil {
		return nil
	}
	return WriteKeychainToken(backup.ServiceName, "claude-code", backup.Token)
}

// SwapOAuthAccount copies the oauthAccount field from the source config dir's
// .claude.json into the target's. Mirrors the Darwin implementation so that
// Claude Code reports the correct accountUuid/organizationUuid after rotation.
func SwapOAuthAccount(targetConfigDir, sourceConfigDir string) (json.RawMessage, error) {
	targetPath := filepath.Join(expandTilde(targetConfigDir), ".claude.json")
	sourcePath := filepath.Join(expandTilde(sourceConfigDir), ".claude.json")

	if _, err := os.Stat(targetPath); os.IsNotExist(err) {
		return nil, nil
	}
	if _, err := os.Stat(sourcePath); os.IsNotExist(err) {
		return nil, nil
	}

	sourceData, err := os.ReadFile(sourcePath)
	if err != nil {
		return nil, fmt.Errorf("reading source .claude.json: %w", err)
	}
	var sourceDoc map[string]json.RawMessage
	if err := json.Unmarshal(sourceData, &sourceDoc); err != nil {
		return nil, fmt.Errorf("parsing source .claude.json: %w", err)
	}
	sourceOAuth, ok := sourceDoc["oauthAccount"]
	if !ok {
		return nil, fmt.Errorf("source .claude.json has no oauthAccount")
	}

	targetData, err := os.ReadFile(targetPath)
	if err != nil {
		return nil, fmt.Errorf("reading target .claude.json: %w", err)
	}
	var targetDoc map[string]json.RawMessage
	if err := json.Unmarshal(targetData, &targetDoc); err != nil {
		return nil, fmt.Errorf("parsing target .claude.json: %w", err)
	}

	backup := targetDoc["oauthAccount"]
	targetDoc["oauthAccount"] = sourceOAuth

	out, err := json.MarshalIndent(targetDoc, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("marshaling target .claude.json: %w", err)
	}
	if err := os.WriteFile(targetPath, out, 0600); err != nil {
		return nil, fmt.Errorf("writing target .claude.json: %w", err)
	}
	return backup, nil
}

// RestoreOAuthAccount writes the backup oauthAccount back to the target.
func RestoreOAuthAccount(targetConfigDir string, backup json.RawMessage) error {
	if backup == nil {
		return nil
	}
	targetPath := filepath.Join(expandTilde(targetConfigDir), ".claude.json")
	data, err := os.ReadFile(targetPath)
	if err != nil {
		return fmt.Errorf("reading target .claude.json: %w", err)
	}
	var doc map[string]json.RawMessage
	if err := json.Unmarshal(data, &doc); err != nil {
		return fmt.Errorf("parsing target .claude.json: %w", err)
	}
	doc["oauthAccount"] = backup
	out, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling target .claude.json: %w", err)
	}
	return os.WriteFile(targetPath, out, 0600)
}

// InspectKeychainToken parses the stored credentials and returns the expiry
// timestamp alongside a validity error. A non-nil error means the token is
// known to be expired (or otherwise definitively unusable). A zero expiry
// means the format was opaque to the parser — the caller should treat the
// token as "unknown" rather than expired.
//
// Always-nil error with zero time is normal for opaque blobs that we cannot
// verify without an HTTP probe (Claude OAuth flow can't be cheaply checked).
func InspectKeychainToken(configDir string) (time.Time, error) {
	raw, err := ReadKeychainToken(configDir)
	if err != nil {
		return time.Time{}, nil
	}
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return time.Time{}, nil
	}

	// Strategy 1: Claude Code's nested {claudeAiOauth:{expiresAt: <ms>}} format.
	var oauthDoc struct {
		ClaudeAiOauth struct {
			ExpiresAt int64 `json:"expiresAt"`
		} `json:"claudeAiOauth"`
	}
	if json.Unmarshal([]byte(raw), &oauthDoc) == nil && oauthDoc.ClaudeAiOauth.ExpiresAt > 0 {
		// expiresAt is in milliseconds since epoch.
		expiry := time.Unix(0, oauthDoc.ClaudeAiOauth.ExpiresAt*int64(time.Millisecond))
		if time.Now().After(expiry) {
			return expiry, fmt.Errorf("token expired at %s", expiry.Format(time.RFC3339))
		}
		return expiry, nil
	}

	// Strategy 2: flat {expires_at: <seconds>} format (matches Darwin parser).
	var flat struct {
		ExpiresAt int64 `json:"expires_at"`
	}
	if json.Unmarshal([]byte(raw), &flat) == nil && flat.ExpiresAt > 0 {
		expiry := time.Unix(flat.ExpiresAt, 0)
		if time.Now().After(expiry) {
			return expiry, fmt.Errorf("token expired at %s", expiry.Format(time.RFC3339))
		}
		return expiry, nil
	}

	// Strategy 3: raw JWT (rare for Claude Code OAuth, kept for parity).
	parts := strings.Split(raw, ".")
	if len(parts) == 3 {
		payload, decErr := base64.RawURLEncoding.DecodeString(parts[1])
		if decErr == nil {
			var claims struct {
				Exp int64 `json:"exp"`
			}
			if json.Unmarshal(payload, &claims) == nil && claims.Exp > 0 {
				expiry := time.Unix(claims.Exp, 0)
				if time.Now().After(expiry) {
					return expiry, fmt.Errorf("JWT expired at %s", expiry.Format(time.RFC3339))
				}
				return expiry, nil
			}
		}
	}

	return time.Time{}, nil
}

// ValidateKeychainToken returns an error only if the stored credentials are
// known to be expired. Opaque tokens are treated as valid (the actual swap
// will surface a clearer error later).
func ValidateKeychainToken(configDir string) error {
	_, err := InspectKeychainToken(configDir)
	return err
}

// SyncSwappedTokens propagates fresh credentials from source accounts to target
// config dirs that were swapped during quota rotation. See the Darwin variant
// for the rationale; this implementation just operates on files instead of
// keychain entries.
func SyncSwappedTokens(swapDirs map[string]string) int {
	updated := 0
	for targetDir, sourceDir := range swapDirs {
		if KeychainServiceName(targetDir) == KeychainServiceName(sourceDir) {
			continue
		}
		targetToken, err := ReadKeychainToken(targetDir)
		if err != nil {
			continue
		}
		sourceToken, err := ReadKeychainToken(sourceDir)
		if err != nil {
			continue
		}
		if targetToken == sourceToken {
			continue
		}
		if err := WriteKeychainToken(targetDir, "claude-code", sourceToken); err != nil {
			continue
		}
		updated++
	}
	return updated
}

// expandTilde expands a leading ~/ to the user's home directory.
func expandTilde(path string) string {
	if strings.HasPrefix(path, "~/") {
		home, err := os.UserHomeDir()
		if err == nil {
			return home + path[1:]
		}
	}
	return path
}
