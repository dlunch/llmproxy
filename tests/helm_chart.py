"""Check both deployments and runtime configuration without a Kubernetes cluster."""
import copy
import json
import pathlib
import subprocess
import tempfile
import tomllib

import yaml

chart = pathlib.Path(__file__).resolve().parents[1] / "charts" / "llmproxy"
defaults = {
    "database": {"existingSecret": "postgres", "key": "url"},
    "dashboard": {
        "apiUrl": "https://api.example.com",
        "origin": "https://dashboard.example.com",
    },
}


def render(values):
    with tempfile.NamedTemporaryFile(mode="w", suffix=".yaml") as file:
        yaml.safe_dump(values, file)
        file.flush()
        return subprocess.run(
            ["helm", "template", "proxy", str(chart), "-f", file.name],
            capture_output=True, text=True,
        )


for section, key in (("database", "existingSecret"), ("dashboard", "apiUrl"), ("dashboard", "origin")):
    values = copy.deepcopy(defaults)
    values[section][key] = ""
    result = render(values)
    assert result.returncode != 0
    assert f"{section}.{key}" in result.stderr, result.stderr

for custom_config in (False, True):
    values = copy.deepcopy(defaults)
    if custom_config:
        values.update({
            "replicaCount": 3,
            "image": {"tag": "api-test"},
            "imagePullSecrets": [{"name": "ghcr"}],
            "config": 'listen = "0.0.0.0:8080"\nrequest_timeout_seconds = 120\n'
            'claude_executable = "/opt/claude"\ncookie_secure = false\n',
        })
        values["dashboard"].update({
            "image": {"repository": "registry.example.com/dashboard", "tag": "dashboard-test"},
            "replicaCount": 4,
            "resources": {"requests": {"memory": "32Mi"}},
            "apiUrl": "https://api.example.com:8443",
            "origin": "https://dashboard.example.com:8443",
        })
    result = render(values)
    assert result.returncode == 0, result.stderr
    manifests = {(item["kind"], item["metadata"]["name"]): item
                 for item in yaml.safe_load_all(result.stdout)}
    assert set(manifests) == {
        ("ConfigMap", "proxy-config"), ("ConfigMap", "proxy-dashboard-config"),
        ("Deployment", "proxy"), ("Deployment", "proxy-dashboard"),
        ("Service", "proxy"), ("Service", "proxy-dashboard"),
    }

    config = tomllib.loads(manifests["ConfigMap", "proxy-config"]["data"]["config.toml"])
    assert config["listen"] == "0.0.0.0:8080"
    assert config["dashboard_origin"] == values["dashboard"]["origin"]
    assert "providers" not in config
    assert config["cookie_secure"] is not custom_config
    assert config["claude_executable"] == ("/opt/claude" if custom_config else "/usr/local/bin/claude")
    frontend_config = json.loads(manifests["ConfigMap", "proxy-dashboard-config"]["data"]["config.json"])
    assert frontend_config == {"apiUrl": values["dashboard"]["apiUrl"]}

    for name, app_name in (("proxy", "llmproxy"), ("proxy-dashboard", "llmproxy-dashboard")):
        deployment = manifests["Deployment", name]["spec"]
        pod = deployment["template"]["spec"]
        container = pod["containers"][0]
        service = manifests["Service", name]["spec"]
        labels = {"app.kubernetes.io/name": app_name, "app.kubernetes.io/instance": "proxy"}
        assert deployment["selector"]["matchLabels"] == labels
        assert deployment["template"]["metadata"]["labels"] == labels
        assert service["selector"] == labels
        assert service["type"] == "ClusterIP"
        assert service["ports"] == [{"name": "http", "port": 8080, "targetPort": "http"}]
        assert deployment["strategy"]["type"] == "RollingUpdate"
        assert pod["automountServiceAccountToken"] is False
        assert pod["affinity"]["nodeAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"] == {
            "nodeSelectorTerms": [{"matchExpressions": [{
                "key": "kubernetes.io/arch", "operator": "In", "values": ["amd64", "arm64"],
            }]}],
        }
        assert pod["securityContext"] == {
            "runAsNonRoot": True, "runAsUser": 1000, "runAsGroup": 1000, "fsGroup": 1000,
        }
        assert container["securityContext"]["allowPrivilegeEscalation"] is False
        assert container["securityContext"]["capabilities"]["drop"] == ["ALL"]
        assert container["ports"] == [{"name": "http", "containerPort": 8080}]
        volumes = {item["name"]: item for item in pod["volumes"]}
        assert volumes["config"]["configMap"]["name"] == f"{name}-config"
        assert volumes["tmp"]["emptyDir"] == {}
        assert len(deployment["template"]["metadata"]["annotations"]["checksum/config"]) == 64
        if custom_config:
            assert pod["imagePullSecrets"] == [{"name": "ghcr"}]
        if name == "proxy":
            assert deployment["replicas"] == (3 if custom_config else 2)
            assert container["image"] == f"ghcr.io/dlunch/llmproxy:{'api-test' if custom_config else 'latest'}"
            assert pod["terminationGracePeriodSeconds"] > config["request_timeout_seconds"] + 30
            env = {item["name"]: item for item in container["env"]}
            assert env["DATABASE_URL"]["valueFrom"]["secretKeyRef"] == {"name": "postgres", "key": "url"}
            assert env["LLMPROXY_CONFIG"]["value"] == "/etc/llmproxy/config.toml"
            probe_path = "/health"
        else:
            assert deployment["replicas"] == (4 if custom_config else 2)
            assert container["image"] == ("registry.example.com/dashboard:dashboard-test"
                                          if custom_config else "ghcr.io/dlunch/llmproxy-dashboard:latest")
            assert container["securityContext"]["readOnlyRootFilesystem"] is True
            assert "env" not in container
            assert container["resources"] == ({"requests": {"memory": "32Mi"}} if custom_config else {})
            assert container["volumeMounts"] == [
                {"name": "config", "mountPath": "/usr/share/nginx/html/config.json",
                 "subPath": "config.json", "readOnly": True},
                {"name": "tmp", "mountPath": "/tmp"},
            ]
            probe_path = "/"
        assert all(container[probe]["httpGet"] == {"path": probe_path, "port": "http"}
                   for probe in ("readinessProbe", "livenessProbe"))

    if not custom_config:
        checksums = {name: manifests["Deployment", name]["spec"]["template"]["metadata"]["annotations"]["checksum/config"]
                     for name in ("proxy", "proxy-dashboard")}

for key, changed_deployment in (("origin", "proxy"), ("apiUrl", "proxy-dashboard")):
    values = copy.deepcopy(defaults)
    values["dashboard"][key] = "https://replacement.example.com"
    result = render(values)
    assert result.returncode == 0, result.stderr
    for item in yaml.safe_load_all(result.stdout):
        if item["kind"] == "Deployment":
            name = item["metadata"]["name"]
            checksum = item["spec"]["template"]["metadata"]["annotations"]["checksum/config"]
            assert (checksum != checksums[name]) == (name == changed_deployment)

print("Helm render checks passed (separate API/dashboard, required origins, configuration rollouts)")
