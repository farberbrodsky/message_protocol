/*!
Our protocol for message size is similar to WebSockets, but not exactly the same.

If the first byte is 0-253, this is the length of the message content.

If the first byte is 254, read the next 2 bytes, this 16 bit number is the length.

If the first byte is 255, read the next 8 bytes, this 64 bit number is the length.
*/
#![crate_name = "message_protocol"]
#![deny(missing_docs)]
use byteorder::{ByteOrder, LittleEndian};
use std::io::{self};

pub fn encode_message(bytes: &[u8]) -> Vec<u8> {
    //! Encodes a binary message so it can be read on the other side.
    let len = bytes.len();
    if len <= 253 {
        let mut msg = vec![0u8; len + 1];
        msg[0] = len as u8;
        msg[1..].clone_from_slice(bytes);
        msg
    } else if len <= 65535 {
        // next 2 bytes are length
        let mut msg = vec![0u8; len + 3];
        msg[0] = 254;
        LittleEndian::write_u16(&mut msg[1..3], len as u16);
        msg[3..].clone_from_slice(bytes);
        msg
    } else {
        // next 8 bytes are length
        let mut msg = vec![0u8; len + 9];
        msg[0] = 255;
        LittleEndian::write_u64(&mut msg[1..9], len as u64);
        msg[9..].clone_from_slice(bytes);
        msg
    }
}

pub fn decode_message<T: Iterator<Item = Result<u8, io::Error>>>(
    mut iter: &mut T,
) -> io::Result<Vec<u8>> {
    //! Decodes a binary message, which was encoded on the other side. May return an error if the
    //! data is invalid.

    fn copy_or_result<T: Iterator<Item = Result<u8, io::Error>>>(
        mut iter: T,
        target: &mut [u8],
        amount: usize,
    ) -> (T, io::Result<()>) {
        for i in 0..amount {
            if let Err(x) = match iter.next() {
                Some(num_or_err) => match num_or_err {
                    Ok(num) => {
                        target[i] = num;
                        Ok(())
                    }
                    Err(x) => Err(x),
                },
                None => Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "stopped in the middle of a message",
                )),
            } {
                return (iter, Err(x));
            }
        }
        (iter, Ok(()))
    }

    struct TakeNoMoveIterator<'a> {
        remaining: usize,
        iter: &'a mut dyn Iterator<Item = Result<u8, io::Error>>,
    }

    impl Iterator for TakeNoMoveIterator<'_> {
        type Item = Result<u8, io::Error>;

        fn next(&mut self) -> Option<Self::Item> {
            if self.remaining == 0 {
                None
            } else {
                self.remaining -= 1;
                self.iter.next()
            }
        }
    }

    let first_byte_option = iter.next();
    if let Some(first_byte) = first_byte_option {
        let message_length = match first_byte {
            Ok(255) => {
                // read next 8 bytes for length
                let mut buf = [0u8; 8];
                let (_, err) = copy_or_result(
                    TakeNoMoveIterator {
                        remaining: 8,
                        iter: &mut iter,
                    },
                    &mut buf,
                    8,
                );
                if let Err(x) = err {
                    return Err(x);
                }
                LittleEndian::read_u64(&buf) as usize
            }
            Ok(254) => {
                // read next 2 bytes for length
                let mut buf = [0u8; 2];
                let (_, err) = copy_or_result(
                    TakeNoMoveIterator {
                        remaining: 2,
                        iter: &mut iter,
                    },
                    &mut buf,
                    2,
                );
                if let Err(x) = err {
                    return Err(x);
                }
                LittleEndian::read_u16(&buf) as usize
            }
            Ok(x) => {
                // read next x bytes, this is the message
                x as usize
            }
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "can't read first byte of message",
                ))
            }
        };
        let mut buf = vec![0u8; message_length];
        let (_, err) = copy_or_result(
            TakeNoMoveIterator {
                remaining: message_length,
                iter: &mut iter,
            },
            &mut buf,
            message_length,
        );
        if let Err(x) = err {
            return Err(x);
        }
        Ok(buf)
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "can't read first byte of message",
        ));
    }
}

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::thread;

/// All events that can come from a client/server.
pub enum Message {
    /// Opened client/server
    Open,
    /// Recieved bytes
    Bytes(Vec<u8>),
    /// Closed client/server
    Close,
}

/// A message recieved from a client
pub struct ClientMessage {
    /// The message
    pub message: Message,
    /// The address of the client.
    pub address: SocketAddr,
    write_tx: mpsc::Sender<Result<Vec<u8>, ()>>,
}

impl ClientMessage {
    /// Write a message to the client
    pub fn write(&self, what: Vec<u8>) {
        self.write_tx.send(Ok(what));
    }
}

/// Listens for TCP on given address asynchronously.
///
/// Returns a Reciever which sends messages of the type Message
///
/// Example:
/// ```
/// let recieve_messages = message_protocol::listen_to_tcp("127.0.0.1:45932")?;
/// while let Ok(msg) = recieve_messages.recv() {
///     println!("from {}", msg.address);
///     match msg.message {
///         message_protocol::Message::Open => println!("open"),
///         message_protocol::Message::Bytes(b) => println!("bytes {:?}", b),
///         message_protocol::Message::Close => println!("close"),
///      };
///  }
/// ```
pub fn listen_to_tcp<T: ToSocketAddrs>(
    bind_to_address: T,
) -> Result<mpsc::Receiver<ClientMessage>, io::Error> {
    let listener = TcpListener::bind(bind_to_address)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // listen for individuals connecting to us
        while let Ok((socket, addr)) = listener.accept() {
            // listen for messages from this individual
            // make a writer handle to the socket
            let mut socket_2 = socket.try_clone().unwrap();
            let (write_tx, write_rx) = mpsc::channel::<Result<Vec<u8>, ()>>();
            let write_thread = thread::spawn(move || {
                'outer: while let Ok(msg_or_stop) = write_rx.recv() {
                    if let Ok(msg) = msg_or_stop {
                        socket_2.write_all(&msg);
                    } else {
                        break 'outer;
                    }
                }
            });
            // reading messages
            let mut iter = socket.bytes();
            let tx = tx.clone();
            tx.send(ClientMessage {
                message: Message::Open,
                address: addr,
                write_tx: write_tx.clone(),
            });
            let write_tx2 = write_tx.clone();
            thread::spawn(move || {
                // listen for messages
                while let Ok(message) = decode_message(&mut iter) {
                    tx.send(ClientMessage {
                        message: Message::Bytes(message),
                        address: addr,
                        write_tx: write_tx2.clone(),
                    });
                }
                tx.send(ClientMessage {
                    message: Message::Close,
                    address: addr,
                    write_tx: write_tx2.clone(),
                });
            });
            write_tx.send(Err(())).unwrap();
            write_thread.join().unwrap();
        }
    });
    Ok(rx)
}

/// Use this to write messages back to the server.
pub struct WriteFunction {
    socket: TcpStream,
}

impl WriteFunction {
    /// The method in WriteFunction that actually sends a message.
    pub fn call(&mut self, what: &[u8]) -> Result<(), io::Error> {
        self.socket.write_all(&encode_message(what))
    }
}

/// Connect to TCP on a given address.
///
/// Example:
/// ```
/// let (rx, mut write) = message_protocol::connect_to_tcp("127.0.0.1:45932")?;
/// write.call(b"hi server").unwrap();
/// while let Ok(msg) = rx.recv() {
///     println!("client recieved");
///     match msg {
///         message_protocol::Message::Open => println!("open"),
///         message_protocol::Message::Bytes(b) => println!("bytes {:?}", b),
///         message_protocol::Message::Close => println!("close"),
///     };
///  }
/// ```
pub fn connect_to_tcp<T: ToSocketAddrs>(
    connect_to_address: T,
) -> Result<(mpsc::Receiver<Message>, WriteFunction), io::Error> {
    let socket = TcpStream::connect(connect_to_address)?;
    let (tx, rx) = mpsc::channel();
    tx.send(Message::Open);
    let mut iter = socket.try_clone().unwrap().bytes();
    thread::spawn(move || {
        // listen for messages
        while let Ok(message) = decode_message(&mut iter) {
            tx.send(Message::Bytes(message));
        }
        tx.send(Message::Close);
    });
    Ok((rx, WriteFunction { socket }))
}
