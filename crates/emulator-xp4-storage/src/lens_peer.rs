//! X-mount lens behind an SPI data FIFO driven by DMA (X-T3 5.11: SPI0,
//! data register 0xfff32008). Each transmitted byte is one byte-wide SPI
//! mode-3 frame of the upstream startup link; its reply byte is received.
use crate::storage::DmaPeer;
use fujimod_emulator_parts_io::serial::{Frame, Wire};
pub use fujimod_emulator_parts_lens::x_mount_link::Configuration;
use fujimod_emulator_parts_lens::x_mount_link::Link;
use serde_json::Value;

pub struct XMountPeer(Link);
impl XMountPeer {
    pub fn new(configuration: Configuration) -> Result<Self, String> {
        Link::new(configuration).map(Self)
    }
}
impl DmaPeer for XMountPeer {
    fn exchange(&mut self, now: u64, transmitted: &[u8]) -> Result<Vec<u8>, String> {
        transmitted
            .iter()
            .map(|&b| {
                let frame = Frame {
                    bits: 8,
                    mode: 3,
                    word: u16::from(b),
                };
                let token = self.0.begin(frame)?;
                self.0.finish(token, frame, now).map(|w| w as u8)
            })
            .collect()
    }
    fn save(&self) -> Value {
        self.0.save()
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        self.0.restore(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lens() -> XMountPeer {
        XMountPeer::new(Configuration {
            peer_parameters: 0xb801,
            parameters: 0xb801,
            identification: (0..128).collect(),
            status: 0x80,
            aperture_ring: 0,
            focus_ring: 0xffff,
            detail_status: 0x14,
            read_catalogs: vec![],
            family_replies: vec![],
            focus_position: None,
        })
        .unwrap()
    }
    #[test]
    fn the_x_t3_sync_header_and_camera_ack_reach_the_upstream_link() {
        // X-T3 5.11 run L4: SPI0 sends SYNC 0x00003fc6, then the camera
        // acknowledgment 0x08108022, big-endian, four bytes per DMA block.
        let mut peer = lens();
        let header = peer.exchange(1, &0x00003fc6u32.to_be_bytes()).unwrap();
        let ack = peer.exchange(2, &0x08108022u32.to_be_bytes()).unwrap();
        let ack = u32::from_be_bytes(ack.try_into().unwrap());
        assert_eq!(header, [0; 4]);
        assert_ne!(ack & 0x8000, 0);
        assert_eq!((ack >> 6) & 0x1ff, 0xff);
        let mut resumed = lens();
        resumed.restore(&peer.save()).unwrap();
        assert_eq!(resumed.save(), peer.save());
        // A request the upstream link does not model stops the run.
        let mut strict = lens();
        assert!(strict.exchange(1, &0x00ff03f4u32.to_be_bytes()).is_err());
    }
    #[test]
    fn a_declared_family_reply_answers_the_x_t3_imgstb_command() {
        use fujimod_emulator_parts_lens::x_mount_link::FamilyReply;
        // X-T3 5.11 run L7C: message 0x1894 sends family 0xa8, payload 2
        // (word 0x00022a00, 0x00022a2e with its check bits). Refused by
        // default, echoed when declared.
        let header = 0x00022a2eu32.to_be_bytes();
        assert!(lens().exchange(1, &header).is_err());
        let mut configuration = Configuration {
            peer_parameters: 0xb801,
            parameters: 0xb801,
            identification: (0..128).collect(),
            status: 0x80,
            aperture_ring: 0,
            focus_ring: 0xffff,
            detail_status: 0x14,
            read_catalogs: vec![],
            family_replies: vec![],
            focus_position: None,
        };
        configuration.family_replies = vec![FamilyReply {
            family: 0xa8,
            payload: None,
        }];
        let mut peer = XMountPeer::new(configuration.clone()).unwrap();
        assert_eq!(peer.exchange(1, &header).unwrap(), [0; 4]);
        let ack = peer.exchange(2, &0x08108022u32.to_be_bytes()).unwrap();
        let ack = u32::from_be_bytes(ack.try_into().unwrap());
        assert_ne!(ack & 0x8000, 0);
        assert_eq!((ack >> 6) & 0x1ff, 0xa8);
        let reply = peer.exchange(3, &[0; 4]).unwrap();
        let reply = u32::from_be_bytes(reply.try_into().unwrap());
        assert_eq!(reply >> 16, 2);
        assert_eq!((reply >> 6) & 0x1ff, 0xa8);
        assert_eq!(reply & 0x8000, 0);
        let mut resumed = XMountPeer::new(configuration).unwrap();
        resumed.restore(&peer.save()).unwrap();
        assert_eq!(resumed.save(), peer.save());
    }
}
