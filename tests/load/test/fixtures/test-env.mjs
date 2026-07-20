// Keep pure load-harness contract tests importable without installation secrets.
// Real orchestrators still require operators to provide RSCTF_JWT_SECRET.
process.env.RSCTF_JWT_SECRET ||= 'rsctf-load-harness-test-only-secret';
