use anyhow::Error;
use core::str;
use dhcproto::v4::{Architecture, Flags};
use dhcproto::v4::{
    Decodable, Decoder, DhcpOption, Encodable, Encoder, Message, MessageType, Opcode, OptionCode,
};
use log::{info, trace};
use std::fmt;
use std::net::Ipv4Addr;
use tokio::net::UdpSocket;

#[derive(Clone, Debug)]
pub struct DhcpPxeResponder {
    architecture: Option<Architecture>,
    user_class: Option<String>,
    redirect_to: Ipv4Addr,
    boot_file: String,
}

#[derive(Clone, Debug)]
pub struct DHCPProxyBuilder {
    responders: Vec<DhcpPxeResponder>,
}

impl DHCPProxyBuilder {
    pub fn new() -> DHCPProxyBuilder {
        DHCPProxyBuilder {
            responders: Vec::new(),
        }
    }

    pub fn add_responder(
        mut self,
        architecture: Option<Architecture>,
        user_class: Option<String>,
        redirect_to: Ipv4Addr,
        boot_file: String,
    ) -> DHCPProxyBuilder {
        self.responders.push(DhcpPxeResponder {
            architecture,
            user_class,
            redirect_to,
            boot_file,
        });
        self
    }

    pub async fn build(self) -> Result<DHCPProxy, Error> {
        return Ok(DHCPProxy::new(self.responders).await?);
    }
}

pub struct DHCPProxy {
    socket: UdpSocket,
    responders: Vec<DhcpPxeResponder>,
    buf: Vec<u8>,
}

impl DHCPProxy {
    pub async fn new(responders: Vec<DhcpPxeResponder>) -> Result<DHCPProxy, Error> {
        let socket = UdpSocket::bind("0.0.0.0:67").await?;
        socket.set_broadcast(true).unwrap();

        Ok(DHCPProxy {
            socket,
            responders,
            buf: vec![0; 1500],
        })
    }

    pub async fn run(self) -> Result<(), Error> {
        let DHCPProxy {
            socket,
            responders,
            mut buf,
        } = self;

        println!("DHCP Listening on: {}", socket.local_addr()?);

        loop {
            let valid_bytes = socket.recv(&mut buf).await?;
            let data = &buf[..valid_bytes];

            let msg = Message::decode(&mut Decoder::new(&data)).unwrap();
            let response = DHCPProxy::handle_packet(msg, responders.clone());
            match response {
                Some(response) => {
                    let mut response_buffer: Vec<u8> = Vec::new();
                    let mut response_encoder = Encoder::new(&mut response_buffer);
                    response.encode(&mut response_encoder)?;
                    socket
                        .send_to(response_buffer.as_slice(), "255.255.255.255:68")
                        .await?;
                }
                _ => {}
            }
        }
    }

    fn handle_packet(message: Message, responders: Vec<DhcpPxeResponder>) -> Option<Message> {
        let options = message.opts();
        let mac_address = message.chaddr();

        let opcode = message.opcode();
        let architecture = options.get(OptionCode::ClientSystemArchitecture);
        let network_interface = options.get(OptionCode::ClientNetworkInterface);
        let vendor_class = options.get(OptionCode::ClassIdentifier);
        let message_type = options.get(OptionCode::MessageType);
        let user_class = options.get(OptionCode::UserClass);
        let requested_params = options.get(OptionCode::ParameterRequestList);

        match (
            opcode,
            message_type,
            requested_params,
            vendor_class,
            architecture,
            network_interface,
        ) {
            (
                Opcode::BootRequest,                         // is a boot request
                Some(DhcpOption::MessageType(message_type)), // option 53 is set
                Some(DhcpOption::ParameterRequestList(_)),   //option 55 is set
                Some(DhcpOption::ClassIdentifier(class_id)), // option 60 is set
                Some(DhcpOption::ClientSystemArchitecture(request_architecture)), // option 93 is set
                Some(DhcpOption::ClientNetworkInterface(_, _, _)), // option 94 is set
            ) => {
                if message_type != &MessageType::Discover && message_type != &MessageType::Request {
                    // message_type(opt 53) must be Discover or Request
                    return None;
                }

                let class_id_str: &str = str::from_utf8(class_id).unwrap_or_default();
                if !class_id_str.starts_with("PXEClient") {
                    // class_id(opt 60) must start with PXEClient
                    return None;
                }

                let request_user_class = match user_class {
                    Some(DhcpOption::UserClass(class)) => {
                        String::from_utf8(class.to_vec()).unwrap_or_default()
                    }
                    Some(_) => String::default(),
                    None => String::default(),
                };

                info!(
                    "DHCP PXEClient {:?} Request from {} ({:?})",
                    message_type,
                    HexSlice::new(mac_address),
                    request_architecture
                );

                let mut redirect_to = Ipv4Addr::new(0, 0, 0, 0);
                let mut boot_file = String::default();

                for responder in responders {
                    match (responder.architecture, responder.user_class) {
                        (None, None) => {
                            redirect_to = responder.redirect_to;
                            boot_file = responder.boot_file;
                        }
                        (Some(arch), None) => {
                            if &arch == request_architecture {
                                redirect_to = responder.redirect_to;
                                boot_file = responder.boot_file;
                            }
                        }
                        (None, Some(class)) => {
                            if class == request_user_class {
                                redirect_to = responder.redirect_to;
                                boot_file = responder.boot_file;
                            }
                        }
                        (Some(arch), Some(class)) => {
                            if &arch == request_architecture && class == request_user_class {
                                redirect_to = responder.redirect_to;
                                boot_file = responder.boot_file;
                            }
                        }
                    }
                }

                info!(
                    "Responding to {} ({:?},{}) with {} ({})",
                    HexSlice::new(mac_address),
                    request_architecture,
                    request_user_class,
                    redirect_to.to_string(),
                    boot_file
                );

                let mut response = Message::default();
                response
                    .set_flags(Flags::default().set_broadcast())
                    .set_chaddr(&mac_address)
                    .set_xid(message.xid())
                    .set_siaddr(redirect_to)
                    .set_sname(redirect_to.to_string().as_bytes())
                    .set_opcode(Opcode::BootReply)
                    .opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Offer));

                let mut vendor_options: Vec<u8> = Vec::new();
                vendor_options.push(6);                                     // Set Option 6
                vendor_options.push(8);                                     // Length 8 Bytes
                vendor_options.append(&mut vec![0, 0, 0, 0, 0, 0, 0, 0]);   // 8 Empty Bytes
                vendor_options.push(255);                                   // PXEClient End

                response
                    .opts_mut()
                    .insert(DhcpOption::VendorExtensions(vendor_options));

                response
                    .opts_mut()
                    .insert(DhcpOption::ServerIdentifier(redirect_to));

                response
                    .opts_mut()
                    .insert(DhcpOption::ClassIdentifier("PXEClient".as_bytes().to_vec()));

                response
                    .opts_mut()
                    .insert(DhcpOption::BootfileName(boot_file.as_bytes().to_vec()));

                Some(response)
            }
            _ => {
                trace!(
                    "receieved Non-PXE DHCP Packet from {}",
                    HexSlice::new(mac_address)
                );
                None
            }
        }
    }
}

struct HexSlice<'a>(&'a [u8]);

impl<'a> HexSlice<'a> {
    fn new<T>(data: &'a T) -> HexSlice<'a>
    where
        T: ?Sized + AsRef<[u8]> + 'a,
    {
        HexSlice(data.as_ref())
    }
}

// You can choose to implement multiple traits, like Lower and UpperHex
impl fmt::Display for HexSlice<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = self.0.len();
        for (pos, byte) in self.0.iter().enumerate() {
            if pos != len - 1 {
                write!(f, "{:X}:", byte)?;
            } else {
                write!(f, "{:X}", byte)?;
            }
        }
        Ok(())
    }
}
