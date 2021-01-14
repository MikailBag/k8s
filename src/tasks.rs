use anyhow::Context as _;
use k8s_openapi::api::{core::v1, rbac::v1 as rbacv1};
use kube::{api::ObjectMeta, Api};
use std::collections::BTreeMap;

fn make_role_rules() -> Vec<rbacv1::PolicyRule> {
    let mut rules = Vec::new();

    let mut allow_standard_ops = |group: &str, kinds: &[&str]| {
        rules.push(rbacv1::PolicyRule {
            api_groups: Some(vec![group.to_string()]),
            verbs: vec![
                "get".to_string(),
                "list".to_string(),
                "create".to_string(),
                "delete".to_string(),
                "watch".to_string(),
            ],
            resources: Some(kinds.iter().copied().map(ToString::to_string).collect()),
            non_resource_urls: None,
            resource_names: None,
        });
    };
    allow_standard_ops("", &["pods", "pods/exec", "pods/attach", "services", "replicasets", "configmaps"]);
    allow_standard_ops("apps/v1", &["deployments"]);

    let mut allow_special_ops = |group: &str, kind: &str, ops: &[&str]| {
        rules.push(rbacv1::PolicyRule {
            api_groups: Some(vec![group.to_string()]),
            verbs: ops.iter().copied().map(ToString::to_string).collect(),
            resources: Some(vec![kind.to_string()]),
            non_resource_urls: None,
            resource_names: None,
        });
    };
    allow_special_ops("", "pods", &["exec", "attach", "logs"]);

    rules
}

pub async fn add_user(name: &str) -> anyhow::Result<()> {
    let k = crate::kube().await?;
    println!("Creating namespace");
    let ns_api = Api::all(k.clone());
    ns_api
        .create(
            &Default::default(),
            &v1::Namespace {
                metadata: ObjectMeta {
                    name: Some(name.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await?;
    println!("Creating role");
    let role_api = Api::namespaced(k.clone(), name);
    role_api
        .create(
            &Default::default(),
            &rbacv1::Role {
                metadata: ObjectMeta {
                    name: Some(name.to_string()),
                    ..Default::default()
                },
                rules: Some(make_role_rules()),
            },
        )
        .await?;
    println!("Creating rolebinding");
    let rolebindings_api = Api::namespaced(k.clone(), name);
    rolebindings_api
        .create(
            &Default::default(),
            &rbacv1::RoleBinding {
                metadata: ObjectMeta {
                    name: Some(name.to_string()),
                    ..Default::default()
                },
                role_ref: rbacv1::RoleRef {
                    api_group: "rbac.authorization.k8s.io".to_string(),
                    kind: "Role".to_string(),
                    name: name.to_string(),
                },
                subjects: Some(vec![rbacv1::Subject {
                    kind: "User".to_string(),
                    name: name.to_string(),
                    ..Default::default()
                }]),
            },
        )
        .await?;
    // let secrets_api = Api::namespaced(client, name.to_string());
    let script = r#"
set -euxo pipefail
apt-get update
apt-get install -y openssl curl
curl -LO "https://storage.googleapis.com/kubernetes-release/release/v1.20.1/bin/linux/amd64/kubectl"
chmod +x ./kubectl
mv ./kubectl /usr/bin/kubectl
kubectl version --client
# kubectl cluster-info

openssl genrsa -out key.pem 4096
openssl req -new -key key.pem -out csr.pem -subj "/CN=$USER/O=people"
openssl x509 -req -in csr.pem -CA /pki/ca.crt -CAkey /pki/ca.key -CAcreateserial -out crt.pem -days 365

# kubectl auth can-i --namespace $USER --list
kubectl create secret generic --from-file=key=key.pem \
                              --from-file=crt=crt.pem \
                              --from-file=csr=csr.pem \
                              --from-file=ca=/pki/ca.crt \
                              credentials
    "#;
    println!("Creating issuer");
    let serviceaccounts_api = Api::namespaced(k.clone(), name);
    serviceaccounts_api
        .create(
            &Default::default(),
            &v1::ServiceAccount {
                metadata: ObjectMeta {
                    name: Some("issuer".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await?;
    role_api
        .create(
            &Default::default(),
            &rbacv1::Role {
                metadata: ObjectMeta {
                    name: Some("issuer".to_string()),
                    ..Default::default()
                },
                rules: Some(vec![rbacv1::PolicyRule {
                    api_groups: Some(vec!["".to_string()]),
                    verbs: vec!["create".to_string()],
                    resources: Some(vec!["secrets".to_string()]),
                    ..Default::default()
                }]),
            },
        )
        .await?;
    rolebindings_api
        .create(
            &Default::default(),
            &rbacv1::RoleBinding {
                metadata: ObjectMeta {
                    name: Some("issuer".to_string()),
                    ..Default::default()
                },
                role_ref: rbacv1::RoleRef {
                    api_group: "rbac.authorization.k8s.io".to_string(),
                    kind: "Role".to_string(),
                    name: "issuer".to_string(),
                },
                subjects: Some(vec![rbacv1::Subject {
                    kind: "User".to_string(),
                    name: format!("system:serviceaccount:{}:issuer", name),
                    ..Default::default()
                }]),
            },
        )
        .await?;
    //xshell::cmd!("kubectl auth can-i --as=issuer --namespace {name} --list").run()?;
    let configmaps_api = Api::namespaced(k.clone(), name);
    let scripts = {
        let s = script.replace("$USER", name);
        let mut m = BTreeMap::new();
        m.insert("issue.sh".to_string(), s);
        m
    };
    configmaps_api
        .create(
            &Default::default(),
            &v1::ConfigMap {
                metadata: ObjectMeta {
                    name: Some("issuer".to_string()),
                    ..Default::default()
                },
                data: Some(scripts),
                ..Default::default()
            },
        )
        .await?;
    let pods_api = Api::namespaced(k.clone(), name);
    pods_api
        .create(
            &Default::default(),
            &v1::Pod {
                metadata: ObjectMeta {
                    name: Some("issuer".to_string()),
                    ..Default::default()
                },
                spec: Some(v1::PodSpec {
                    volumes: Some(vec![
                        v1::Volume {
                            name: "pki".to_string(),
                            host_path: Some(v1::HostPathVolumeSource {
                                path: "/etc/kubernetes/pki".to_string(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        v1::Volume {
                            name: "scripts".to_string(),
                            config_map: Some(v1::ConfigMapVolumeSource {
                                name: Some("issuer".to_string()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ]),
                    containers: vec![v1::Container {
                        name: "main".to_string(),
                        image: Some("ubuntu:focal".to_string()),
                        args: Some(vec!["sleep".to_string(), "3600".to_string()]),
                        volume_mounts: Some(vec![
                            v1::VolumeMount {
                                name: "pki".to_string(),
                                mount_path: "/pki".to_string(),
                                ..Default::default()
                            },
                            v1::VolumeMount {
                                name: "scripts".to_string(),
                                mount_path: "/scripts".to_string(),
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    }],
                    termination_grace_period_seconds: Some(1),
                    service_account_name: Some("issuer".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    crate::configure_kubectl();

    xshell::cmd!("kubectl wait --namespace {name} --for=condition=Ready pod/issuer").run()?;
    xshell::cmd!("kubectl exec --namespace {name} issuer -- bash /scripts/issue.sh").run()?;
    println!("Cleaning up issuer");
    if true {
        pods_api.delete("issuer", &Default::default()).await?;
        configmaps_api.delete("issuer", &Default::default()).await?;
        rolebindings_api
            .delete("issuer", &Default::default())
            .await?;
        role_api.delete("issuer", &Default::default()).await?;
        serviceaccounts_api
            .delete("issuer", &Default::default())
            .await?;
    }
    println!("Fetching certificates");
    let secrets_api = Api::<v1::Secret>::namespaced(k.clone(), name);
    let creds = secrets_api.get("credentials").await?;
    let kubeconfig = {
        let get = |name: &str| -> anyhow::Result<String> {
            let bin_data = creds
                .data
                .as_ref()
                .context("secret data missing")?
                .get(name)
                .with_context(|| format!("secret does not have field {}", name))?
                .0
                .clone();
            String::from_utf8(bin_data).context("field is not utf8")
        };

        let our_config: serde_yaml::Value =
            serde_yaml::from_str(&xshell::read_file(crate::ROOT.join("state/kubeconfig"))?).context("failed to parse local kubeconfig")?;
            let our_config: serde_json::Value =
            serde_yaml::from_value(our_config)?;
        let server = our_config
            .pointer("/clusters/0/cluster/server")
            .context("server missing")?
            .as_str()
            .context("server is not string")?;
        serde_json::json!({
            "apiVersion": "v1",
            "kind": "Config",
            "clusters": [
                {
                    "name": "d-k8s",
                    "cluster": {
                        "certificate-authority-data": base64::encode(get("ca")?),
                        "server": server,
                    }
                }
            ],
            "users": [
                {
                    "name": name,
                    "user": {
                        "client-certificate-data": base64::encode(get("crt")?),
                        "client-key-data": base64::encode(get("key")?)
                    }
                }
            ],
            "contexts": [
                {
                    "name": "d-k8s",
                    "context": {
                        "cluster": "d-k8s",
                        "user": name,
                    }
                }
            ],
            "current-context": "d-k8s"
        })
    };
    let kubeconfig = serde_json::to_string(&kubeconfig)?;

    let out_path = "/tmp/kubeconfig";

    xshell::write_file(out_path, kubeconfig)?;

    println!("Kubeconfig for user '{}' is written to {}", name, out_path);
    Ok(())
}
