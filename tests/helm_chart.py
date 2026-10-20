"""Check the rendered deployment contract without a Kubernetes cluster."""
import pathlib
import subprocess
import tempfile
import tomllib

import yaml

chart = pathlib.Path(__file__).resolve().parents[1] / "charts" / "llmproxy"
missing_secret = subprocess.run(
    ["helm", "template", "proxy", str(chart)], capture_output=True, text=True
)
assert missing_secret.returncode != 0
assert "database.existingSecret" in missing_secret.stderr

for with_claude in (False, True):
    values = {"database": {"existingSecret": "postgres", "key": "url"}}
    if with_claude:
        values.update({
            "nodeSelector": {"kubernetes.io/arch": "amd64"},
            "claude": {"existingClaim": "claude-binary"},
            "imagePullSecrets": [{"name": "ghcr"}],
            "config": 'listen = "0.0.0.0:8080"\nrequest_timeout_seconds = 120\n'
            '[[providers]]\nkind = "claude"\nid = "claude-main"\n'
            'executable = "/var/lib/llmproxy/claude/bin/claude"\n',
        })
    with tempfile.NamedTemporaryFile(mode="w", suffix=".yaml") as file:
        yaml.safe_dump(values, file)
        file.flush()
        output = subprocess.check_output(
            ["helm", "template", "proxy", str(chart), "-f", file.name], text=True
        )
    manifests = {item["kind"]: item for item in yaml.safe_load_all(output)}
    assert set(manifests) == {"ConfigMap", "Deployment", "Service"}
    config = tomllib.loads(manifests["ConfigMap"]["data"]["config.toml"])
    deployment = manifests["Deployment"]["spec"]
    pod = deployment["template"]["spec"]
    container = pod["containers"][0]
    env = {item["name"]: item for item in container["env"]}
    assert config["listen"] == "0.0.0.0:8080"
    assert deployment["replicas"] == 1 and deployment["strategy"]["type"] == "Recreate"
    assert pod["terminationGracePeriodSeconds"] > config["request_timeout_seconds"] + 30
    assert env["DATABASE_URL"]["valueFrom"]["secretKeyRef"] == {"name": "postgres", "key": "url"}
    assert env["LLMPROXY_CONFIG"]["value"] == "/etc/llmproxy/config.toml"
    assert pod["securityContext"]["runAsUser"] == 1000
    assert manifests["Service"]["spec"]["selector"] == deployment["selector"]["matchLabels"]
    assert deployment["template"]["metadata"]["labels"] == deployment["selector"]["matchLabels"]
    assert all(container[probe]["httpGet"] == {"path": "/health", "port": "http"}
               for probe in ("readinessProbe", "livenessProbe"))
    volumes = {item["name"]: item for item in pod["volumes"]}
    assert volumes["config"]["configMap"]["name"] == manifests["ConfigMap"]["metadata"]["name"]
    assert volumes["tmp"]["emptyDir"] == {}
    assert ("claude" in volumes) == with_claude
    if with_claude:
        assert volumes["claude"]["persistentVolumeClaim"]["claimName"] == "claude-binary"
        mount = next(item for item in container["volumeMounts"] if item["name"] == "claude")
        assert config["providers"][0]["executable"].startswith(mount["mountPath"] + "/")
        assert mount["readOnly"] is True
        assert pod["nodeSelector"] == {"kubernetes.io/arch": "amd64"}
        assert pod["imagePullSecrets"] == [{"name": "ghcr"}]
print("Helm render checks passed (Codex default and Claude executable PVC)")
