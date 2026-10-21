"""Deploy the SystemDeck server to a Docker host over SSH.

Uploads the workspace source, docker-compose.yml and Caddyfile to ~/<dir> on the
host, then runs `docker compose up -d --build` there (the image is built on the
host). Server data in ~/<dir>/data is never touched. HTTPS is served by Caddy with
its internal CA; the CA root is saved to release/ and published as a dashboard
download (SystemDeck-CA.crt) so PCs can trust it.

  pip install paramiko
  Uses your SSH key if the host accepts it; otherwise asks for the password
  (or reads SYSTEMDECK_DEPLOY_PASS).
  python deploy/deploy.py --host 192.168.202.38 --user sysadmin
  python deploy/deploy.py --host ... --user ... --ai-network ollama_default --ai-url http://ollama:11434/v1 --ai-model qwen3:8b

Options --ai-* only seed the AI settings on a fresh database; after that they are
managed in the dashboard (Settings).
"""
import argparse
import getpass
import io
import os
import posixpath
import sys
import tarfile
import time

import paramiko

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # the SystemDeck workspace
SOURCES = ["Cargo.toml", "Cargo.lock", "Dockerfile", ".dockerignore", "shared", "server", "client", "vendor"]
SKIP_DIRS = {"target", "data", ".git", "release"}
SKIP_EXT = (".key", ".redb", ".sdrec", ".sdweb")


def source_tarball() -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for item in SOURCES:
            path = os.path.join(ROOT, item)
            if os.path.isfile(path):
                tar.add(path, arcname=item)
                continue
            for dirpath, dirnames, filenames in os.walk(path):
                dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
                for name in filenames:
                    if name.endswith(SKIP_EXT):
                        continue
                    full = os.path.join(dirpath, name)
                    tar.add(full, arcname=os.path.relpath(full, ROOT).replace(os.sep, "/"))
    return buf.getvalue()


class Host:
    def __init__(self, host, user):
        self.ssh = paramiko.SSHClient()
        self.ssh.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        try:
            # SSH key (~/.ssh/id_* or the agent) first: no password needed.
            self.ssh.connect(host, username=user, timeout=15)
        except paramiko.AuthenticationException:
            password = os.environ.get("SYSTEMDECK_DEPLOY_PASS") or getpass.getpass(f"SSH password for {user}@{host}: ")
            self.ssh.connect(host, username=user, password=password, timeout=15, allow_agent=False, look_for_keys=False)
        self.sftp = self.ssh.open_sftp()
        self.home = self.sftp.normalize(".")

    def run(self, cmd, check=True, quiet=False):
        _, stdout, stderr = self.ssh.exec_command(cmd, timeout=3600)
        out = []
        for line in iter(stdout.readline, ""):
            out.append(line)
            if not quiet:
                sys.stdout.write("  " + line)
        err = stderr.read().decode(errors="replace")
        code = stdout.channel.recv_exit_status()
        if err and not quiet:
            sys.stdout.write("".join("  " + l + "\n" for l in err.rstrip().splitlines()))
        if check and code != 0:
            raise SystemExit(f"remote command failed ({code}): {cmd}")
        return code, "".join(out)

    def put_bytes(self, data: bytes, remote):
        with self.sftp.open(remote, "wb") as f:
            f.write(data)

    def put(self, local, remote):
        self.sftp.put(local, remote)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--host", required=True)
    ap.add_argument("--user", required=True)
    ap.add_argument("--dir", default="systemdeck", help="deploy folder under the user's home (default: systemdeck)")
    ap.add_argument("--public-host", help="address clients use (default: --host)")
    ap.add_argument("--ai-network", help="external Docker network to join, e.g. ollama_default")
    ap.add_argument("--ai-url", help="seed AI endpoint, e.g. http://ollama:11434/v1")
    ap.add_argument("--ai-model", help="seed AI model, e.g. qwen3:8b")
    args = ap.parse_args()
    public = args.public_host or args.host

    h = Host(args.host, args.user)
    d = posixpath.join(h.home, args.dir)
    print(f"Deploying to {args.user}@{args.host}:{d}")
    h.run(f"mkdir -p {d}/data {d}/downloads")

    print("Uploading source...")
    tar = source_tarball()
    h.put_bytes(tar, f"{d}/src.tar.gz")
    h.run(f"rm -rf {d}/src && mkdir {d}/src && tar xzf {d}/src.tar.gz -C {d}/src && rm {d}/src.tar.gz")
    print(f"  {len(tar) // 1024} KB")

    h.put(os.path.join(ROOT, "deploy", "docker-compose.yml"), f"{d}/docker-compose.yml")
    h.put(os.path.join(ROOT, "deploy", "Caddyfile"), f"{d}/Caddyfile")
    h.put_bytes(f"SYSTEMDECK_HOST={public}\n".encode(), f"{d}/.env")

    # Optional: join an existing network (e.g. Ollama's) and seed AI settings.
    if args.ai_network or args.ai_url or args.ai_model:
        lines = ["services:", "  systemdeck:"]
        env = {k: v for k, v in (("SYSTEMDECK_AI_URL", args.ai_url), ("SYSTEMDECK_AI_MODEL", args.ai_model)) if v}
        if env:
            lines.append("    environment:")
            lines += [f"      {k}: \"{v}\"" for k, v in env.items()]
        if args.ai_network:
            lines += ["    networks: [default, ai]", "networks:", "  ai:", "    external: true", f"    name: {args.ai_network}"]
        h.put_bytes(("\n".join(lines) + "\n").encode(), f"{d}/docker-compose.override.yml")
    else:
        h.run(f"rm -f {d}/docker-compose.override.yml")

    downloads = os.path.join(ROOT, "release", "SystemDeck-Server", "downloads")
    if os.path.isdir(downloads):
        for name in sorted(os.listdir(downloads)):
            full = os.path.join(downloads, name)
            if os.path.isfile(full):
                print(f"Uploading download {name}...")
                h.put(full, f"{d}/downloads/{name}")

    print("Building and starting containers (first build takes a few minutes)...")
    h.run(f"cd {d} && docker compose up -d --build --remove-orphans 2>&1")

    print("Waiting for HTTPS...")
    for _ in range(30):
        code, out = h.run("curl -sk -o /dev/null -w '%{http_code}' https://127.0.0.1/api/health", check=False, quiet=True)
        if out.strip() == "200":
            break
        time.sleep(2)
    print(f"  https health: {out.strip() or 'no answer'}")

    code, ca = h.run(f"cd {d} && docker compose exec -T caddy cat /data/caddy/pki/authorities/local/root.crt", check=False, quiet=True)
    if code == 0 and "BEGIN CERTIFICATE" in ca:
        local = os.path.join(ROOT, "release", f"systemdeck-ca-{public}.crt")
        os.makedirs(os.path.dirname(local), exist_ok=True)
        with open(local, "w", newline="\n") as f:
            f.write(ca)
        h.put_bytes(ca.encode(), f"{d}/downloads/SystemDeck-CA.crt")
        print(f"CA root -> {local} (also on the dashboard as SystemDeck-CA.crt)")

    h.run(f"cd {d} && docker compose ps --format 'table {{{{.Name}}}}\\t{{{{.Status}}}}\\t{{{{.Ports}}}}'")
    _, logs = h.run(f"cd {d} && docker compose logs --no-color systemdeck 2>&1 | grep -A2 'First start' || true", quiet=True)
    if logs.strip():
        print("First start - admin account:\n" + logs)
    print(f"\nDashboard: https://{public}/")


if __name__ == "__main__":
    main()
