# Gevulot cli tool

Gevulot cli tool allows to send commands / transactions to a Gevulot node.

## Available commands

The commands provided by gevulot-cli are:
 * **generate-key** : Generate a secret key in the specified file. The secret key is used to sign the Tx sent to the node.
 * **deploy**: Send a deploy Tx to the node to deploy the specified prover / verifier.
 * **exec** : Send a run Tx to be executed by the node.
 * **calculate-hash**: Calculate the Hash of the specified file.

 For detailed information use : gevulot-cli --help

### Common parameters

 * -j, --jsonurl: URL of the node JSON-RPC access point. Default: http://localhost:9944
 * -k, --keyfile: Path to a secret key file. Default: localkey.pki

### Deploy command

This command allows to deploy a compiled prover / verified image to the node specified with the --jsonurl parameter.

The deployment can be done using file installed on the same host as the gevulot-cli tool. In this case the --prover and / or --verifier contains the path to the image to deploy. The Gevulot node try to open an http connection to the gevulot-cli host. Use `--listen-addr` to specify the http bind address used by gevulot-cli (default 127.0.0.1:8080). 
If the host that executes gevulot-cli cannot be reached by the node using http, the image can be installed on an http host that is visible to the node and the url can be specified. In this case the --prover and / or --verifier parameters contains the hash of the image. Use the **calculate-hash** command to get the Hash of an image file. To specify the distant image http url use the --proverimgurl and / or --verifierimgurl 

For more information use: `gevulot-cli deploy --help`

#### Examples:

To deploy local images: 
```
gevulot-cli -- deploy --name test --prover ./prover --verifier ./verifier
```

To deploy a local prover and a distant verifier: 
```
gevulot-cli -- deploy --name test --prover ../../target/debug/prover --verifier 491907d04032869088ef9b81004639ed1bb185f0413a261f74faaa0aa943d3f3 --verifierimgurl http://...
```

### Exec command

This command allows to execute a workflow of task on the nodes. A task is an execution of a program on the node. Deployed prover or verifier are programs.

The task are defined using a json format. Use --tasks parameter to specify a list of execution tasks or steps.

#### Example of steps:

To execute the program with the specified hash with arguments --nonce=42: 
```
{"program":"9616d42b0d82c1ed06eab8eaa26680261ad831012bbf3ad8303738a53bf85c7c","cmd_args":[{"name":"--nonce","value":"42"}],"inputs":[]}
```

To execute a program with input of type output for the program data: 
```
{"program":"37ef718f473a96e2dd56ac27fc175bfa08f4a30e34bdff5802e2f5071265a942", "cmd_args":[],"inputs":[{"Output":{"source_program":"9616d42b0d82c1ed06eab8eaa26680261ad831012bbf3ad8303738a53bf85c7c","file_name":"/workspace/proof.dat"}}]}
```

A command using these steps:
```
gelulot-cli -- exec --tasks '[{"program":"9616d42b0d82c1ed06eab8eaa26680261ad831012bbf3ad8303738a53bf85c7c","cmd_args":[{"name":"--nonce","value":"42"}],"inputs":[]},{"program":"37ef718f473a96e2dd56ac27fc175bfa08f4a30e34bdff5802e2f5071265a942","cmd_args":[],"inputs":[{"Output":{"source_program":"9616d42b0d82c1ed06eab8eaa26680261ad831012bbf3ad8303738a53bf85c7c","file_name":"/workspace/proof.dat"}}]}]'
```

## License

This library is licensed under either of the following licenses, at your discretion.

[Apache License Version 2.0](LICENSE-APACHE)

[MIT License](LICENSE-MIT)

Any contribution that you submit to this library shall be dual licensed as above (as defined in the Apache v2 License), without any additional terms or conditions.
