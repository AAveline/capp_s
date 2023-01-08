pub mod js;
pub mod yaml;
use crate::serializer::{
    BuildContext, ContainerAppBluePrint, ContainerAppConfiguration, ContainerBluePrint,
    ContainerImageBluePrint, DaprBluePrint, IngressBluePrint, Language, Serializer,
};
use log::error;
use regex::Regex;

pub struct Pulumi {
    language: Language,
    pub resources: Option<Vec<ContainerAppConfiguration>>,
}

impl Pulumi {
    pub fn new(language: Language) -> Option<Pulumi> {
        match language {
            Language::Yaml | Language::Typescript | Language::Javascript => Some(Pulumi {
                language,
                resources: None,
            }),
            _ => None,
        }
    }
}

impl Serializer for Pulumi {
    type Output = Pulumi;
    fn deserialize_value(&mut self, input: &str) -> Option<&Self> {
        match self.language {
            Language::Yaml => match yaml::deserialize(input) {
                Some(value) => {
                    self.resources = Some(value);
                    Some(self)
                }
                None => None,
            },
            Language::Typescript | Language::Javascript => match js::deserialize(input) {
                Some(value) => {
                    self.resources = Some(value);
                    Some(self)
                }
                None => None,
            },
            _ => {
                error!("Language not supported");
                None
            }
        }
    }
}

#[derive(Debug, PartialEq)]
struct Resource {
    name: String,
    property: Option<String>,
    is_reference: bool,
}

#[derive(Debug, PartialEq)]
pub struct DockerImageForPulumi {
    name: Option<String>,
    path: Option<String>,
    is_context: bool,
}

#[derive(Debug)]
pub struct AppConfiguration {
    pub container: ContainerBluePrint,
    pub dapr_configuration: Option<DaprBluePrint>,
    pub ingress_configuration: Option<IngressBluePrint>,
}

fn extract_and_parse_resource_name(s: String) -> Result<Resource, ()> {
    let is_reference = s.contains("${");
    match Regex::new(r"\$\{(.+)\.(.+)\}")
        .expect("Should match previous regex")
        .captures(&s)
    {
        Some(v) => {
            let name = v.get(1).map_or("", |m| m.as_str()).to_string();
            let property = Some(v.get(2).map_or("", |m| m.as_str()).to_string());
            Ok(Resource {
                name,
                property,
                is_reference,
            })
        }
        None => Ok(Resource {
            name: s,
            property: None,
            is_reference,
        }),
    }
}

fn check_and_match_reference(
    images: &Vec<ContainerImageBluePrint>,
    resource: Resource,
) -> Option<DockerImageForPulumi> {
    let name = resource.name;
    let val = images
        .iter()
        .find(|image| image.referenceName.clone().unwrap() == name);
    let re = Regex::new(r"(\$\{.+\})(/)(.+)").unwrap();

    match val {
        Some(val) => {
            let has_build_context = &val.build;

            let a = re.captures(&has_build_context.context).unwrap();

            let image_name = a.get(3).map_or("", |m| m.as_str());
            let context_path = a.get(1).map_or("", |m| m.as_str());

            if image_name.is_empty() | context_path.is_empty() {
                return None;
            }

            Some(DockerImageForPulumi {
                name: None,
                path: Some(format!(
                    "{}/{}",
                    context_path.replace("${pulumi.cwd}", "."),
                    image_name
                )),
                is_context: true,
            })
        }
        None => {
            if resource.is_reference {
                None
            } else {
                Some(DockerImageForPulumi {
                    name: Some(name.to_string()),
                    is_context: false,
                    path: None,
                })
            }
        }
    }
}

fn build_image_for_serialization(
    images: &Vec<ContainerImageBluePrint>,
    container: ContainerBluePrint,
) -> Option<DockerImageForPulumi> {
    let resource =
        extract_and_parse_resource_name(container.image).expect("Should contains name property");

    check_and_match_reference(images, resource)
}

fn build_ports_mapping_for_serialization(
    configuration: AppConfiguration,
) -> (Option<u32>, Option<Vec<String>>) {
    let dapr_configuration = configuration.dapr_configuration;
    let ingress_configuration = configuration.ingress_configuration;
    let container_name = configuration.container.name;

    let has_dapr_enabled = match &dapr_configuration {
        Some(v) => v.enabled.is_some() && v.enabled.unwrap() == true,
        None => false,
    };

    let has_ingress_exposed = match &ingress_configuration {
        Some(v) => v.external.is_some() && v.external.unwrap() == true,
        None => false,
    };

    let dapr_app_port = match dapr_configuration.clone() {
        Some(val) => val.appPort,
        None => None,
    };

    let dapr_app_id = match dapr_configuration.clone() {
        Some(val) => val.appId,
        None => None,
    };

    let ingress_app_port = match ingress_configuration {
        Some(val) => val.targetPort,
        None => None,
    };

    let mut ports: Vec<String> = vec![];
    // TODO: Assert for now than source and target ports are sames (container name and dapr target)

    if has_dapr_enabled && has_ingress_exposed {
        let has_right_target = container_name == dapr_app_id.unwrap_or_default();

        if has_right_target {
            ports.push(format!(
                "{}:{}",
                ingress_app_port.unwrap_or_default().to_string(),
                dapr_app_port.unwrap_or_default().to_string()
            ))
        }
    }

    if (!has_dapr_enabled) && has_ingress_exposed {
        ports.push(format!(
            "{}:{}",
            ingress_app_port.unwrap_or_default().to_string(),
            ingress_app_port.unwrap_or_default().to_string()
        ))
    }

    (
        dapr_app_port,
        if !ports.is_empty() { Some(ports) } else { None },
    )
}

fn parse_app_configuration(
    images: &Vec<ContainerImageBluePrint>,
    configuration: AppConfiguration,
) -> Option<Vec<ContainerAppConfiguration>> {
    let container = configuration.container.clone();
    let dapr_configuration = configuration.dapr_configuration.clone();

    let image = build_image_for_serialization(images, container)?;
    let name = configuration.container.name.clone();
    let (dapr_app_port, ports) = build_ports_mapping_for_serialization(configuration);

    let has_dapr_enabled = match dapr_configuration {
        Some(v) => v.enabled.unwrap(),
        None => false,
    };

    let result = if has_dapr_enabled {
        vec![
            ContainerAppConfiguration {
                image: image.name,
                build: image.is_context.then(|| BuildContext {
                    context: image.path.unwrap(),
                }),
                name: name.clone(),
                depends_on: Some(vec!["placement".to_string()]),
                networks: Some(vec![String::from("dapr-network")]),
                network_mode: None,
                environment: None,
                ports: ports.clone(),
                command: None,
            },
            // Dapr Sidecar config
            ContainerAppConfiguration {
                image: Some(String::from("daprio/daprd:edge")),
                name: format!("{}_dapr", name.clone()),
                depends_on: Some(vec![String::from(&name)]),
                network_mode: Some(format!("service:{}", String::from(&name))),
                environment: None,
                // No exposed ports for dapr sidecar
                ports: None,
                networks: None,
                build: None,
                command: Some(vec![
                    "./daprd".to_string(),
                    "-app-id".to_string(),
                    String::from(&name),
                    "-app-port".to_string(),
                    format!("{}", dapr_app_port.unwrap_or_default()),
                    "-placement-host-address".to_string(),
                    "placement:50006".to_string(),
                    "air".to_string(),
                ]),
            },
        ]
    } else {
        vec![ContainerAppConfiguration {
            image: image.name,
            build: image.is_context.then(|| BuildContext {
                context: image.path.unwrap(),
            }),
            name,
            depends_on: None,
            // No Dapr network
            networks: None,
            environment: None,
            network_mode: None,
            ports: ports.clone(),
            command: None,
        }]
    };

    Some(result)
}

pub fn build_configuration(
    apps: Vec<ContainerAppBluePrint>,
    images: Vec<ContainerImageBluePrint>,
) -> Vec<ContainerAppConfiguration> {
    let mut services: Vec<ContainerAppConfiguration> = Vec::new();

    for app in apps {
        let containers = app.template.containers;
        let dapr_configuration = app.configuration.dapr;
        let ingress_configuration = app.configuration.ingress;

        let mut a: Vec<ContainerAppConfiguration> = containers
            .unwrap()
            .iter()
            .flat_map(|container| {
                parse_app_configuration(
                    &images,
                    AppConfiguration {
                        container: container.to_owned(),
                        dapr_configuration: dapr_configuration.clone(),
                        ingress_configuration: ingress_configuration.clone(),
                    },
                )
            })
            .flatten()
            .collect();

        services.append(&mut a);
    }
    services
}

mod tests {
    use crate::serializer::{BuildContextBluePrint, ConfigurationBluePrint, TemplateBluePrint};

    use super::*;
    #[test]
    fn test_extract_and_parse_resource_name() {
        let input1 = "${resource.property}".to_string();
        let expected = Ok(Resource {
            name: "resource".to_string(),
            property: Some("property".to_string()),
            is_reference: true,
        });
        let output = extract_and_parse_resource_name(input1);
        assert_eq!(expected, output);

        let input2 = "resource".to_string();
        let expected = Ok(Resource {
            name: "resource".to_string(),
            property: None,
            is_reference: false,
        });
        let output = extract_and_parse_resource_name(input2);
        assert_eq!(expected, output);
    }

    #[test]
    fn test_build_image_for_serialization() {
        // Container with a reference to an existing image with build context
        let container = ContainerBluePrint {
            image: "${myImage.name}".to_string(),
            name: "myapp".to_string(),
        };
        let images = vec![ContainerImageBluePrint {
            name: "myImage".to_string(),
            build: BuildContextBluePrint {
                context: "${pulumi.cwd}/node-app".to_string(),
            },
            referenceName: Some("myImage".to_string()),
        }];

        let output = build_image_for_serialization(&images, container).unwrap();

        let expected = DockerImageForPulumi {
            name: None,
            path: Some("./node-app".to_string()),
            is_context: true,
        };

        assert_eq!(expected, output);

        // Container with a reference to an non-existing image
        let container = ContainerBluePrint {
            image: "${referenceDoNotMatch.name}".to_string(),
            name: "myapp".to_string(),
        };
        let images = vec![ContainerImageBluePrint {
            name: "myImage".to_string(),
            build: BuildContextBluePrint {
                context: "${pulumi.cwd}/node-app".to_string(),
            },
            referenceName: Some("myImage".to_string()),
        }];

        let output = build_image_for_serialization(&images, container);

        assert_eq!(None, output);

        // Container with a remote image without context
        let container = ContainerBluePrint {
            image: "node-12".to_string(),
            name: "myapp".to_string(),
        };
        let images = vec![ContainerImageBluePrint {
            name: "myImage".to_string(),
            build: BuildContextBluePrint {
                context: "${pulumi.cwd}/node-app".to_string(),
            },
            referenceName: Some("myImage".to_string()),
        }];

        let output = build_image_for_serialization(&images, container).unwrap();

        let expected = DockerImageForPulumi {
            name: Some("node-12".to_string()),
            path: None,
            is_context: false,
        };

        assert_eq!(expected, output);
    }

    #[test]
    fn test_build_ports_mapping_for_serialization() {
        // Assert that None dapr and ingress generate None ports
        let container = ContainerBluePrint {
            image: "${myImage.name}".to_string(),
            name: "some-app".to_string(),
        };

        let dapr_configuration = None;
        let ingress_configuration = None;

        let configuration = AppConfiguration {
            container,
            dapr_configuration,
            ingress_configuration,
        };

        let (dapr_app_port, ports) = build_ports_mapping_for_serialization(configuration);

        assert_eq!(dapr_app_port, None);
        assert_eq!(ports, None);

        // Assert that dapr.enabled:false generate None ports
        let container = ContainerBluePrint {
            image: "${myImage.name}".to_string(),
            name: "some-app".to_string(),
        };

        let dapr_configuration = Some(DaprBluePrint {
            appPort: Some(80),
            enabled: Some(false),
            appId: Some("t".to_string()),
        });
        let ingress_configuration = None;

        let configuration = AppConfiguration {
            container,
            dapr_configuration,
            ingress_configuration,
        };

        let (dapr_app_port, ports) = build_ports_mapping_for_serialization(configuration);

        assert_eq!(dapr_app_port, Some(80));
        assert_eq!(ports, None);

        //TODO
        // Assert that dapr.enabled:true without ingress generate None ports
        let container = ContainerBluePrint {
            image: "${myImage.name}".to_string(),
            name: "some-app".to_string(),
        };

        let dapr_configuration = Some(DaprBluePrint {
            appPort: Some(80),
            enabled: Some(true),
            appId: Some("t".to_string()),
        });
        let ingress_configuration = None;

        let configuration = AppConfiguration {
            container,
            dapr_configuration,
            ingress_configuration,
        };

        let (dapr_app_port, ports) = build_ports_mapping_for_serialization(configuration);

        assert_eq!(dapr_app_port, Some(80));
        assert_eq!(ports, None);

        // Assert that dapr.enabled:true with ingress generate None ports if appId doesn't match with existing container
        let container = ContainerBluePrint {
            image: "${myImage.name}".to_string(),
            name: "t".to_string(),
        };

        let dapr_configuration = Some(DaprBluePrint {
            appPort: Some(80),
            enabled: Some(true),
            appId: Some("some-app".to_string()),
        });
        let ingress_configuration = Some(IngressBluePrint {
            external: Some(true),
            targetPort: Some(3000),
        });

        let configuration = AppConfiguration {
            container,
            dapr_configuration,
            ingress_configuration,
        };

        let (dapr_app_port, ports) = build_ports_mapping_for_serialization(configuration);

        assert_eq!(dapr_app_port, Some(80));
        assert_eq!(ports, None);

        // Assert that dapr.enabled:true with ingress generate ports if appId match with existing container
        let container = ContainerBluePrint {
            image: "${myImage.name}".to_string(),
            name: "some-app".to_string(),
        };

        let dapr_configuration = Some(DaprBluePrint {
            appPort: Some(80),
            enabled: Some(true),
            appId: Some("some-app".to_string()),
        });
        let ingress_configuration = Some(IngressBluePrint {
            external: Some(true),
            targetPort: Some(3000),
        });

        let configuration = AppConfiguration {
            container,
            dapr_configuration,
            ingress_configuration,
        };

        let (dapr_app_port, ports) = build_ports_mapping_for_serialization(configuration);

        assert_eq!(dapr_app_port, Some(80));
        assert_eq!(ports, Some(vec!["3000:80".to_string()]));

        // Assert that dapr.enabled:false with ingress.enabled:true  generate  Ingress ports
        let container = ContainerBluePrint {
            image: "${myImage.name}".to_string(),
            name: "some-app".to_string(),
        };

        let dapr_configuration = Some(DaprBluePrint {
            appPort: Some(80),
            enabled: Some(false),
            appId: Some("t".to_string()),
        });
        let ingress_configuration = Some(IngressBluePrint {
            external: Some(true),
            targetPort: Some(3000),
        });

        let configuration = AppConfiguration {
            container,
            dapr_configuration,
            ingress_configuration,
        };

        let (dapr_app_port, ports) = build_ports_mapping_for_serialization(configuration);

        assert_eq!(dapr_app_port, Some(80));
        assert_eq!(ports, Some(vec!["3000:3000".to_string()]));
    }

    #[test]
    fn test_parse_app_configuration() {
        let configuration = AppConfiguration {
            container: ContainerBluePrint {
                image: "${myImage.name}".to_string(),
                name: "myapp".to_string(),
            },
            dapr_configuration: Some(DaprBluePrint {
                appPort: Some(3000),
                enabled: Some(true),
                appId: Some("myapp".to_string()),
            }),
            ingress_configuration: Some(IngressBluePrint {
                external: Some(true),
                targetPort: Some(80),
            }),
        };

        let images = vec![ContainerImageBluePrint {
            name: "${registry.loginServer}/node-app:v1.0.0".to_string(),
            build: BuildContextBluePrint {
                context: "${pulumi.cwd}/node-app".to_string(),
            },
            referenceName: Some("myImage".to_string()),
        }];

        let output = parse_app_configuration(&images, configuration);

        let expected = vec![
            ContainerAppConfiguration {
                image: None,
                build: Some(BuildContext {
                    context: "./node-app".to_string(),
                }),
                name: "myapp".to_string(),
                depends_on: Some(vec!["placement".to_string()]),
                networks: Some(vec![String::from("dapr-network")]),
                network_mode: None,
                environment: None,
                ports: Some(vec!["80:3000".to_string()]),
                command: None,
            },
            ContainerAppConfiguration {
                image: Some(String::from("daprio/daprd:edge")),
                name: format!("myapp_dapr"),
                depends_on: Some(vec![String::from("myapp")]),
                network_mode: Some(format!("service:{}", String::from("myapp"))),
                environment: None,
                ports: None,
                networks: None,
                build: None,
                command: Some(vec![
                    "./daprd".to_string(),
                    "-app-id".to_string(),
                    String::from("myapp"),
                    "-app-port".to_string(),
                    "3000".to_string(),
                    "-placement-host-address".to_string(),
                    "placement:50006".to_string(),
                    "air".to_string(),
                ]),
            },
        ];

        assert_eq!(Some(expected), output);

        let configuration = AppConfiguration {
            container: ContainerBluePrint {
                image: "node-12".to_string(),
                name: "myapp".to_string(),
            },
            dapr_configuration: Some(DaprBluePrint {
                appPort: Some(3000),
                enabled: Some(false),
                appId: Some("myapp".to_string()),
            }),
            ingress_configuration: Some(IngressBluePrint {
                external: Some(false),
                targetPort: Some(80),
            }),
        };

        let images = vec![ContainerImageBluePrint {
            name: "${registry.loginServer}/node-app:v1.0.0".to_string(),
            build: BuildContextBluePrint {
                context: "${pulumi.cwd}/node-app".to_string(),
            },
            referenceName: Some("myImage".to_string()),
        }];

        let output = parse_app_configuration(&images, configuration);

        let expected = vec![ContainerAppConfiguration {
            image: Some("node-12".to_string()),
            build: None,
            name: "myapp".to_string(),
            depends_on: None,
            networks: None,
            network_mode: None,
            environment: None,
            ports: None,
            command: None,
        }];

        assert_eq!(Some(expected), output);
    }
}
