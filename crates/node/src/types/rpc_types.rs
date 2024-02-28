use crate::types::file::Output;
use crate::types::file::TxFile;
use crate::types::Hash;
use serde::Deserialize;
use std::net::SocketAddr;

use serde::Serialize;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TransactionOutputFile {
    //uri of the file. File can be retrieve using the node HTTP server with the url: http://<host:port><uri>
    url: String,
    //checksum hex encoded of the file for verification
    checksum: String,
    //Path of the file inside the VM. Use to help to recognize it.
    vm_path: String,
}

impl TransactionOutputFile {
    pub fn from_txfile(
        file: TxFile<Output>,
        tx_hash: Hash,
        scheme: &str,
        host: SocketAddr,
    ) -> Self {
        //use parse to create an URL, no new method.
        let mut url = reqwest::Url::parse(&format!("{}localhost", scheme))
            .unwrap_or(reqwest::Url::parse("http://localhost").unwrap()); //unwrap always succeed
        url.set_ip_host(host.ip()).unwrap(); //unwrap always succeed
        url.set_port(Some(host.port())).unwrap(); //unwrap always succeed
        url.set_path(&file.clone().into_download_file(tx_hash).get_uri());

        println!("{url:?}",);
        TransactionOutputFile {
            //uri of the file. File can be retrieve using the node HTTP server with the url: http://<host:port><uri>
            url: url.to_string(),
            //checksum hex encoded of the file for verification
            checksum: file.checksum.to_string(),
            //Path of the file inside the VM. Use to help to recognize it.
            vm_path: file.name,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TransactionOutput {
    pub tx_hash: String,
    //Proof or Verification
    pub kind: String,
    //List of files that correspond to the files fields of  Payload::Proof { files, .. } | Payload::Verification { files, .. }
    pub files: Vec<TransactionOutputFile>,
    //Base64 encoded Payload::Proof { proof, .. } | Payload::Verification { verification, .. }
    //It correspond to the binary output of the Tx execution.
    pub data: String,
}
