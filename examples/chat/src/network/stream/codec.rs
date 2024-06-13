use bytes::{Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub data: Vec<u8>,
}

#[derive(Debug, Error)]
#[error("CodecError")]
pub enum CodecError {
    StdIo(std::io::Error),
    EncodeDecode(std::io::Error),
    SerDe(serde_json::Error),
}

impl From<std::io::Error> for CodecError {
    fn from(err: std::io::Error) -> Self {
        CodecError::StdIo(err)
    }
}

#[derive(Debug)]
pub(crate) struct MessageJsonCodec;

impl Decoder for MessageJsonCodec {
    type Item = Message;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let mut length_codec = LengthDelimitedCodec::new();
        let frame = match length_codec.decode(src) {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(None),
            Err(e) => return Err(CodecError::EncodeDecode(e)),
        };

        match serde_json::from_slice(&frame) {
            Ok(frame) => Ok(Some(frame)),
            Err(e) => Err(CodecError::SerDe(e)),
        }
    }
}

impl Encoder<Message> for MessageJsonCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let mut length_codec = LengthDelimitedCodec::new();
        let json = match serde_json::to_vec(&item) {
            Ok(json) => json,
            Err(e) => return Err(CodecError::SerDe(e)),
        };

        match length_codec.encode(Bytes::from(json), dst) {
            Ok(()) => Ok(()),
            Err(err) => Err(CodecError::EncodeDecode(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio_test::io::Builder;
    use tokio_util::codec::FramedRead;

    #[test]
    fn test_my_frame_encoding_decoding() {
        let request = Message {
            data: "Hello".bytes().collect(),
        };
        let response = Message {
            data: "World".bytes().collect(),
        };

        let mut buffer = BytesMut::new();
        let mut codec = MessageJsonCodec;
        codec.encode(request.clone(), &mut buffer).unwrap();
        codec.encode(response.clone(), &mut buffer).unwrap();

        let decoded_request = codec.decode(&mut buffer).unwrap();
        assert_eq!(decoded_request, Some(request));

        let decoded_response = codec.decode(&mut buffer).unwrap();
        assert_eq!(decoded_response, Some(response));
    }

    #[tokio::test]
    async fn test_multiple_objects_stream() {
        let request = Message {
            data: "Hello".bytes().collect(),
        };
        let response = Message {
            data: "World".bytes().collect(),
        };

        let mut buffer = BytesMut::new();
        let mut codec = MessageJsonCodec;
        codec.encode(request.clone(), &mut buffer).unwrap();
        codec.encode(response.clone(), &mut buffer).unwrap();

        let mut stream = Builder::new().read(&buffer.freeze()).build();
        let mut framed = FramedRead::new(&mut stream, MessageJsonCodec);

        let decoded_request = framed.next().await.unwrap().unwrap();
        assert_eq!(decoded_request, request);

        let decoded_response = framed.next().await.unwrap().unwrap();
        assert_eq!(decoded_response, response);

        let decoded3 = framed.next().await;
        assert_eq!(decoded3.is_none(), true);
    }
}
