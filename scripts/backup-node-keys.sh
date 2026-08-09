#!/usr/bin/env bash
#
# backup-node-keys.sh — encrypted, off-machine-ready backup of an OpenFiat
# node operator's Solana keypairs.
#
# A node's keypairs are the only copy of its identity and — for the wallets
# that hold authority (program-upgrade, sale-admin, treasury) — of real funds.
# A lost disk with no backup is a permanently lost key. This script produces a
# single passphrase-encrypted archive you copy off the machine (hardware token,
# password-manager vault, offline drive), so a lost disk is a recoverable event
# rather than a terminal one.
#
# It is deliberate about what it does NOT do:
#   * It never transmits anything. The output is a local file; moving it
#     off-machine is your manual step (the one place a human must stay in the
#     loop for a secret).
#   * The passphrase is read by gpg's own prompt — never a command-line
#     argument (which leaks via `ps` and shell history), never echoed, never
#     written to disk.
#   * Plaintext key material is only ever unpacked inside a 0700 temp dir that
#     is shredded on exit, including on error or Ctrl-C.
#   * It refuses to finish unless a full decrypt-and-compare round-trip proves
#     the archive actually restores — a backup you cannot restore is not a
#     backup.
#
# Usage:
#   scripts/backup-node-keys.sh [KEY_PATH ...]
#
# With no arguments it backs up the standard custody set:
#   ~/.config/solana/id.json         (the CLI/authority keypair)
#   ~/openfiat-node-keys/            (node + faucet wallets, if present)
# Pass explicit paths (files or directories) to override.
#
# Output (in $OPENFIAT_BACKUP_DIR, default the current directory):
#   openfiat-keys-<host>-<UTC>.tar.gz.gpg   the encrypted archive (0600)
#   openfiat-keys-<host>-<UTC>.manifest.txt the pubkeys + archive checksum
#                                           (safe to keep in the clear)
#
# Restore:
#   gpg --decrypt openfiat-keys-<host>-<UTC>.tar.gz.gpg | tar -xzv
#
set -euo pipefail
umask 077

readonly CIPHER="AES256"
readonly STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
readonly HOST="$(hostname -s 2>/dev/null || echo node)"
readonly OUT_DIR="${OPENFIAT_BACKUP_DIR:-$PWD}"
readonly BASENAME="openfiat-keys-${HOST}-${STAMP}"
readonly ARCHIVE="${OUT_DIR}/${BASENAME}.tar.gz.gpg"
readonly MANIFEST="${OUT_DIR}/${BASENAME}.manifest.txt"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v gpg >/dev/null || die "gpg is required (apt-get install gnupg)."
command -v tar >/dev/null || die "tar is required."

mkdir -p "$OUT_DIR" || die "cannot create output directory: $OUT_DIR"

# A single 0700 workspace for every plaintext byte, wiped unconditionally on
# exit. shred where available, rm as a floor.
readonly WORK="$(mktemp -d "${TMPDIR:-/tmp}/openfiat-keybak.XXXXXX")"
cleanup() {
  if [ -d "$WORK" ]; then
    find "$WORK" -type f -exec shred -u {} + 2>/dev/null || true
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT INT TERM

# Resolve the key set: explicit args, or the standard custody paths.
declare -a SOURCES=()
if [ "$#" -gt 0 ]; then
  SOURCES=("$@")
else
  [ -f "${HOME}/.config/solana/id.json" ] && SOURCES+=("${HOME}/.config/solana/id.json")
  [ -d "${HOME}/openfiat-node-keys" ] && SOURCES+=("${HOME}/openfiat-node-keys")
fi
[ "${#SOURCES[@]}" -gt 0 ] || die "no key paths found — pass them explicitly."
for src in "${SOURCES[@]}"; do
  [ -e "$src" ] || die "no such path: $src"
done

# Stage a copy that preserves permissions, so the restored keys come back 0600.
readonly STAGE="${WORK}/${BASENAME}"
mkdir -p "$STAGE"
for src in "${SOURCES[@]}"; do
  cp -a "$src" "$STAGE/"
done

# Manifest: derive the PUBLIC key of every keypair we can, so a future restore
# can be verified without ever decrypting the archive. Pubkeys are public by
# definition — nothing secret is written here.
{
  printf '# OpenFiat node key backup manifest\n'
  printf '# host=%s created=%s cipher=%s\n' "$HOST" "$STAMP" "$CIPHER"
  printf '# Public keys of the backed-up keypairs (for restore verification):\n'
  if command -v solana-keygen >/dev/null; then
    while IFS= read -r -d '' kp; do
      if pub="$(solana-keygen pubkey "$kp" 2>/dev/null)"; then
        printf '%s  %s\n' "$pub" "${kp#"$STAGE"/}"
      fi
    done < <(find "$STAGE" -type f -name '*.json' -print0)
  else
    printf '# (solana-keygen not on PATH — pubkeys omitted)\n'
  fi
} > "$MANIFEST"

# Obtain the passphrase ONCE. In the encrypt step gpg's stdin is the tar
# stream, so the passphrase cannot come from stdin — we read it here (with
# confirmation) and hand it to gpg over a dedicated file descriptor via a
# pipe, never argv (which `ps` would leak) and never a temp file. Setting
# OPENFIAT_BACKUP_PASSPHRASE bypasses the prompt for automated backups; that
# trades interactive safety for convenience, so use it only with care.
if [ -n "${OPENFIAT_BACKUP_PASSPHRASE:-}" ]; then
  PASS="$OPENFIAT_BACKUP_PASSPHRASE"
else
  read -rs -p "Passphrase: " PASS; echo >&2
  read -rs -p "Confirm   : " PASS_CONFIRM; echo >&2
  [ "$PASS" = "$PASS_CONFIRM" ] || die "passphrases did not match."
  unset PASS_CONFIRM
fi
[ -n "$PASS" ] || die "empty passphrase."

# gpg reads the passphrase from fd 3, fed by a pipe (process substitution) so
# it never touches argv, the environment gpg sees, or disk.
printf 'Encrypting %d source(s) with %s...\n' "${#SOURCES[@]}" "$CIPHER" >&2
tar -C "$WORK" -czf - "$BASENAME" \
  | gpg --symmetric --cipher-algo "$CIPHER" \
        --batch --pinentry-mode loopback --passphrase-fd 3 \
        --no-symkey-cache \
        --output "$ARCHIVE" 3< <(printf '%s' "$PASS")
chmod 600 "$ARCHIVE"

# Round-trip proof: decrypt into a fresh dir with the same passphrase and
# compare byte-for-byte, so we prove the archive actually restores — not
# merely that gpg wrote a file.
printf 'Verifying the archive restores...\n' >&2
readonly VERIFY="${WORK}/verify"
mkdir -p "$VERIFY"
gpg --decrypt --batch --pinentry-mode loopback --passphrase-fd 3 \
    --no-symkey-cache "$ARCHIVE" 3< <(printf '%s' "$PASS") \
  | tar -C "$VERIFY" -xzf -
if ! diff -r "$STAGE" "${VERIFY}/${BASENAME}" >/dev/null; then
  rm -f "$ARCHIVE"
  die "round-trip verification FAILED — archive removed. Do not trust it."
fi

# Record the archive checksum in the manifest now that it is final.
sha256sum "$ARCHIVE" | sed "s#${OUT_DIR}/##" >> "$MANIFEST"

cat >&2 <<EOF

Backup complete and restore-verified.
  archive : $ARCHIVE
  manifest: $MANIFEST

Next (manual, on purpose): copy the archive to at least two locations OFF this
machine. Keep the passphrase somewhere separate from the archive.

Restore with:
  gpg --decrypt "$ARCHIVE" | tar -xzv
EOF
