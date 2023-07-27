use crate::{StunClass, SOFTWARE};
use super::{verify_message, Context, Response};

use std::net::SocketAddr;

use anyhow::Result;
use bytes::BytesMut;
use faster_stun::{Kind, MessageReader, MessageWriter, Method};
use faster_stun::attribute::{ErrKind, Error, ErrorCode, Realm, Software, XorPeerAddress};
use faster_stun::attribute::ErrKind::{BadRequest, Forbidden, Unauthorized};

/// return create permission error response
#[inline(always)]
fn reject<'a>(
    ctx: Context,
    reader: MessageReader,
    bytes: &'a mut BytesMut,
    err: ErrKind,
) -> Result<Option<Response<'a>>> {
    let method = Method::CreatePermission(Kind::Error);
    let mut pack = MessageWriter::extend(method, &reader, bytes);
    pack.append::<ErrorCode>(Error::from(err));
    pack.append::<Realm>(&ctx.env.realm);
    pack.flush(None)?;
    Ok(Some(Response::new(bytes, StunClass::Message, None)))
}

/// return create permission ok response
#[inline(always)]
fn resolve<'a>(
    reader: &MessageReader,
    key: &[u8; 16],
    bytes: &'a mut BytesMut,
) -> Result<Option<Response<'a>>> {
    let method = Method::CreatePermission(Kind::Response);
    let mut pack = MessageWriter::extend(method, reader, bytes);
    pack.append::<Software>(SOFTWARE);
    pack.flush(Some(key))?;
    Ok(Some(Response::new(bytes, StunClass::Message, None)))
}

enum Ret {
    Next,
    Failed,
    Relay,
}

#[inline(always)]
fn check_addr(ctx: &Context, peer: &SocketAddr) -> Ret {
    if ctx.env.external.ip() == peer.ip() {
        return Ret::Next;
    }

    ctx.env
        .proxy
        .as_ref()
        .map(|proxy| {
            proxy
                .get_online_node(&peer.ip())
                .map(|_| Ret::Relay)
                .unwrap_or(Ret::Failed)
        })
        .unwrap_or(Ret::Failed)
}

/// process create permission request
///
/// [rfc8489](https://tools.ietf.org/html/rfc8489)
///
/// When the server receives the CreatePermission request, it processes
/// as per [Section 5](https://tools.ietf.org/html/rfc8656#section-5)
/// plus the specific rules mentioned here.
///
/// The message is checked for validity.  The CreatePermission request
/// MUST contain at least one XOR-PEER-ADDRESS attribute and MAY contain
/// multiple such attributes.  If no such attribute exists, or if any of
/// these attributes are invalid, then a 400 (Bad Request) error is
/// returned.  If the request is valid, but the server is unable to
/// satisfy the request due to some capacity limit or similar, then a 508
/// (Insufficient Capacity) error is returned.
///
/// If an XOR-PEER-ADDRESS attribute contains an address of an address
/// family that is not the same as that of a relayed transport address
/// for the allocation, the server MUST generate an error response with
/// the 443 (Peer Address Family Mismatch) response code.
///
/// The server MAY impose restrictions on the IP address allowed in the
/// XOR-PEER-ADDRESS attribute; if a value is not allowed, the server
/// rejects the request with a 403 (Forbidden) error.
///
/// If the message is valid and the server is capable of carrying out the
/// request, then the server installs or refreshes a permission for the
/// IP address contained in each XOR-PEER-ADDRESS attribute as described
/// in [Section 9](https://tools.ietf.org/html/rfc8656#section-9).  
/// The port portion of each attribute is ignored and may be any arbitrary
/// value.
///
/// The server then responds with a CreatePermission success response.
/// There are no mandatory attributes in the success response.
///
/// > NOTE: A server need not do anything special to implement
/// idempotency of CreatePermission requests over UDP using the
/// "stateless stack approach".  Retransmitted CreatePermission
/// requests will simply refresh the permissions.
pub async fn process<'a, 'b, 'c>(
    ctx: Context,
    reader: MessageReader<'a, 'b>,
    bytes: &'c mut BytesMut,
) -> Result<Option<Response<'c>>> {
    let (username, key) = match verify_message(&ctx, &reader).await {
        None => return reject(ctx, reader, bytes, Unauthorized),
        Some(ret) => ret,
    };

    let peer = match reader.get::<XorPeerAddress>() {
        None => return reject(ctx, reader, bytes, BadRequest),
        Some(a) => a,
    };

    match check_addr(&ctx, &peer) {
        Ret::Failed => return reject(ctx, reader, bytes, Forbidden),
        Ret::Relay => return resolve(&reader, &key, bytes),
        Ret::Next => (),
    }

    if ctx.env.router.bind_port(&ctx.addr, peer.port()).is_none() {
        return reject(ctx, reader, bytes, Forbidden);
    }

    ctx.env
        .observer
        .create_permission(&ctx.addr, username, &peer);
    resolve(&reader, &key, bytes)
}
