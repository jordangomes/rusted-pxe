mod dhcp;

use actix_files as fs;
use actix_web::{middleware::Logger, App, HttpServer};
use anyhow::Error;
use async_tftp::server::TftpServerBuilder;
use dhcp::DHCPProxyBuilder;
use dhcproto::v4::Architecture;
use log::{error, info, trace};
use std::net::Ipv4Addr;

#[actix_web::main]
async fn main() -> Result<(), Error> {
    fern::Dispatch::new()
        .level(log::LevelFilter::Info)
        .level_for("async_tftp", log::LevelFilter::Info)
        .chain(std::io::stdout())
        .apply()
        .expect("Failed to initialize logger");

    // Setup DHCP Server
    actix_rt::spawn(async move {
        loop {
            info!("Starting DHCP Server");
            let server_address = Ipv4Addr::new(10, 0, 0, 5);

            let dhcp_proxy = DHCPProxyBuilder::new()
                .add_responder(
                    Some(Architecture::BC),
                    None,
                    server_address,
                    "ipxe.efi".to_string(),
                )
                .add_responder(
                    Some(Architecture::Intelx86PC),
                    None,
                    server_address,
                    "undionly.kpxe".to_string(),
                )
                .add_responder(
                    None,
                    Some("iPXE".to_string()),
                    server_address,
                    "http://10.0.0.5/boot.ipxe".to_string(),
                )
                .build()
                .await;

            match dhcp_proxy {
                Ok(dhcp_proxy) => match dhcp_proxy.run().await {
                    Ok(_) => {
                        trace!("DHCP Server Running")
                    }
                    Err(err) => {
                        error!("DHCP Server Error! - {err}");
                    }
                },
                Err(err) => {
                    error!("Error Starting DHCP Server! - {err}");
                }
            }

            // wait 5 seconds before attempting to restart server
            info!("Restarting DHCP Server in 5 seconds");
            actix_rt::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    });

    // Setup TFTP Server
    actix_rt::spawn(async move {
        loop {
            info!("Starting TFTP Server");
            let tftpd = TftpServerBuilder::with_dir_ro("./tftp_root")
                .unwrap()
                .bind("0.0.0.0:69".parse().unwrap())
                .block_size_limit(1024)
                .build()
                .await;

            match tftpd {
                Ok(tftpd) => match tftpd.serve().await {
                    Ok(_) => {
                        trace!("TFTP Server Running")
                    }
                    Err(err) => {
                        error!("TFTP Server Error! - {err}");
                    }
                },
                Err(err) => {
                    error!("Error Starting TFTP Server! - {err}");
                }
            }
            // wait 5 seconds before attempting to restart server
            info!("Restarting TFTP Server in 5 seconds");
            actix_rt::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    });

    // Setup HTTP Server
    HttpServer::new(|| {
        App::new()
            .wrap(Logger::default())
            .service(fs::Files::new("/", "./http_root"))
    })
    .bind(("0.0.0.0", 80))?
    .run()
    .await
    .map_err(anyhow::Error::from)
}
