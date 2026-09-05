# Generate fresh cryptographic keys from the kernel's entropy pool. On any
# normal machine these are unrepeatable by design; under the deterministic
# hypervisor the same run produces the same keys, so a bug hiding behind a
# "random" key or nonce replays exactly.
echo "--- ed25519 ---"
openssl genpkey -algorithm ed25519 2>/dev/null | openssl pkey -pubout | openssl sha256
echo "--- rsa-2048 ---"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 2>/dev/null \
    | openssl pkey -pubout | openssl sha256
echo "--- 32-byte session nonce ---"
openssl rand -hex 32
