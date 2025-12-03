# Noise Protocol Communication Architecture

## Overview

This document describes how peers (Coordinator and Attestors) communicate securely using the Noise Protocol Framework during the multi-party decentralized namespace setup process.

**Implementation Details:**
- **Noise Pattern**: NN_PSK2 (not XX)
- **Cryptographic Curve**: secp256k1 (not Curve25519)
- **Libraries**: `tokio-noise` and `hyper-noise` with `secp256k1`
- **Authentication**: Pre-shared keys derived via ECDH from static keypairs

## Visual Workflow

[![](https://mermaid.ink/img/pako:eNq1Wntv2zYQ_yqEihXpFqex7Dws7AHX3qNoqxh1hmJLjEGWaFuILAmS3DZL8913FEWKpGhLSjT_k4i--91Dd8c70g-GG3nYsIxVEH1xN06SoevpbYjgM8_g6egm_7N4hXq9n9FkE0Up_hQld4T8gT4i9vx4G1JOmYxwfovCZeQknh-uv6Er_n-OfVM-cyjrx2Xy-udJgp0Moyl2cZglTuD_iz00A5b7xV5BbkRI3Sz9hibsXyqGPypSpjgOonv0e_QZJ6ETurhkXDCLvvsO_aT5oCv7zdX44_St_Tv6dPXx3W_vrz5pCRmOYnru1Ks3_9iRn-I5znbxTf4vmiVRFrlRYKEZxgma-qlL1LtHL9EfTuilG-cOc-UkAAYJNoTYzfpgdkQEOlmUIJxmzjLw0w1Oc9OpMHjtQBugL362QeMsAyqg7S_q0c0no5sN0O0no9uib5gnGPp4l22IV9OHcRDk_qWAZB3izHch5rxfHlV-s8KvUtgaCk7D1_Io_QtDfMLqbOOkuH-T_0F9C73DEIg4xImT-VG42MNtR99kxwlhyqEKGEwwS1WpQO7orScHyDKJHM91UkiSz76DcglFLl59-DC2pyUugZXcDFgMF2jI1_2bMpoUhXLM0NniNHYg435A0_GH9-iOYu5HNEtEsxtEu0S02yByTGYrw53j0FONRyksVlzqFN_34h0EttsjyCdLP8y_jKE8-K4fO2HW8z2yvFBFmqpI0Tv_j0hbFSm6ryuRXCh3ZVkYggASjaw9iGGbwA7hfy4KgwNZTYSUijxWEE0NYoXI1hIpmQZhWGxTULPjKHWCFB2JukVhcP9KKBUciqHnOOYNh2M4FFcqA5SUq5XL5WLFRK4qRbc6ew5hPDNnKJbWJ-D_KESDk-EPFvUd3i6x58F264eEflGxe2Chub8OEUCWxskKSQYObjgf0YLwwi6omDZgLLDhZdNQMgl5sJb4yx2EkxpjXpj-E5Mtk0QQyiJEYoAFnpStBTAPY1ADHuVCxezizlvUsMtVqTW7XIJ07GVoFvqKaajor8_CFBix1wNH9dirlxOcmaIg15eUpsi2glxfOfYjS6kq-qNIL1g6XB-Ifwm8k-0SnD6qeGYVTyWxdSQ8R-a75dbPcjH1xUCMiJxPiXxLhGNRQVuW9TrBa5LnL1FKaSD4aTZXU3ZYBBfkc2kNEygl6_CGc5BioU_WoZisMzNumqyxGbdIVgAW0wUeNclKlORlZ1HDr8nWVvyadFX5pdgnKouxr5hwMPbBVzz202paEWsU6MYJWw9tK9CNM1YHLeWP6JIiCWDpcMoSH-9LWdEPJZ5KYutI1JT9DeQHDZJWjIucM2fUpi2FlHdiTeYSA6vZK-MLKefSoWNP-16Mtm_nkyvb_nVyLQV1wVq6YxsHOMNH4iTOFtmMXBnBiw7DW7yqG5JBheuP48n1vNmMLI_t9Nzh-pkjsgTAIDsbkQ-hP39EPoTexYgseIKhtxqRBVsr_CqFraHgNJoRGVbVEXk6_oj-jAMI9MUeTjIeS07TjccUgqCVSgrDMVH5CcMxRZ06wj5WIDFUSiHvYYIuOdwqibbIA5DXsI9CIQSy-8UBNHlHey6avL81RON4hX38Pbt39ZvdePJuoQCYAkD9lqYBsAWA-o2LApTxBFqXmZbXe1iq6SkBI30UIUwNhPi9rf2-Ol_OEgxzMqbbSZrCRLh_xJTRmAB1xCyRCnBxzuT0nJvKF8TL25xGP-rUIIhIpUD5lA-N4Evk0ql0l7KK8mbnBx5ih7bwz3ZLivf-eVMQUqpbUVAyXBg9BcPlppbTMk7S-wF146aWquD1Ui6h973U-pSYTATRAB41_axgyaKGW9PNtuDW9LIyN-dnynIASCNYa9DJlg4p2zeh3xRMUbAbtLKNsW0Fu0EvewC7RC-8oGQyLNUUi0onK5itAVNpbC1Ndd779St2IVxblg6CJmWQMA9SxGq1GDKGQqRcIjR6KG1w6RDeEecU0BG_x94aOrzx7K3oegZZ5uuTOmKJtfQq64jLSyO5IdbdFiF6k9SkJZ5f__WeXBod6oOBV-50URh54J4jKDPFK0uz-wArdygrPwisF6vVCD7HUK-iO2y9cN1T-BSPvS--l22sQfz1GFDh9byA7xRA3hEyOBc-teRmO3K7IXnZ3knG7aUXXuiTvSFCyh14Bw4WW-4GLhA77FbkdkPyVg6WM-aZDoYgF9OV7f-v-VZLS9Yy2GE15vPOlYofjVx3teLiT0_Pz8FcSbx5KNrZuWx3aOR44Mlo6svsyFChC3imavDW-PYJFTuGd0QKE9RyHKqvqbzEoUKJSC6FoI5Gaqgc9G55hdMtnt0hXn4k3Smc2S1cl8bmh3mdwpndwj3DWLVOkrmwA93YfNgZlN0RFO9nO4Uzu4V79vsUy1cUFz-2gPoV-OtNhnR7Db_kp5Jxf3UGhXNv4PEL-lbkdkNyfgnYitxsR95GGaECNCQ325EfUEYJEXb81ABeOGtqQ203pOYDcytysx15rWPk_qpHxj0p5GM_vKs0Vso1PmvxiJADr4rfJLahp7caezkUqzUHPQ2ECcNijaDcW_kITE6I4sgPYao7usdBEH2peEn4JQeDXQ1x_9DUw697WzGUvaSGodL4l4d_DYQoM3-NFH6ysMGOR6YFfcdXHKNTMG-IPc_Zb2Nx1NiCeNCGeLifWI2txlqXB6QtiAdtiGu1hjfBf_Wa5tdkxdlAvEviQNm-lN-cFhXjXOr9R6PBgD_W78XyFV0XiPIveZ-MaBwb68T3DCtLdvjY2OJk65BH44FIuzWyDd7iW8OCfz28cnZBdmvcho_AFjvh31G0ZZxJtFtvDGsFBRCedrEHJXHqO-vEKUmgHcHJJNqFmWGd5QiG9WB8NSzzbHBiDs7Ny7OBeW6ORufmsXFvWL3R2cnl5ag_PD03L0YXZ_3Lx2Pj31xo_-T89PRiMLroXwwvB2fm6eDYwJ4PNfsD_X12_jPtx_8AiXAdKQ?type=png)](https://mermaid.live/edit#pako:eNq1Wntv2zYQ_yqEihXpFqex7Dws7AHX3qNoqxh1hmJLjEGWaFuILAmS3DZL8913FEWKpGhLSjT_k4i--91Dd8c70g-GG3nYsIxVEH1xN06SoevpbYjgM8_g6egm_7N4hXq9n9FkE0Up_hQld4T8gT4i9vx4G1JOmYxwfovCZeQknh-uv6Er_n-OfVM-cyjrx2Xy-udJgp0Moyl2cZglTuD_iz00A5b7xV5BbkRI3Sz9hibsXyqGPypSpjgOonv0e_QZJ6ETurhkXDCLvvsO_aT5oCv7zdX44_St_Tv6dPXx3W_vrz5pCRmOYnru1Ks3_9iRn-I5znbxTf4vmiVRFrlRYKEZxgma-qlL1LtHL9EfTuilG-cOc-UkAAYJNoTYzfpgdkQEOlmUIJxmzjLw0w1Oc9OpMHjtQBugL362QeMsAyqg7S_q0c0no5sN0O0no9uib5gnGPp4l22IV9OHcRDk_qWAZB3izHch5rxfHlV-s8KvUtgaCk7D1_Io_QtDfMLqbOOkuH-T_0F9C73DEIg4xImT-VG42MNtR99kxwlhyqEKGEwwS1WpQO7orScHyDKJHM91UkiSz76DcglFLl59-DC2pyUugZXcDFgMF2jI1_2bMpoUhXLM0NniNHYg435A0_GH9-iOYu5HNEtEsxtEu0S02yByTGYrw53j0FONRyksVlzqFN_34h0EttsjyCdLP8y_jKE8-K4fO2HW8z2yvFBFmqpI0Tv_j0hbFSm6ryuRXCh3ZVkYggASjaw9iGGbwA7hfy4KgwNZTYSUijxWEE0NYoXI1hIpmQZhWGxTULPjKHWCFB2JukVhcP9KKBUciqHnOOYNh2M4FFcqA5SUq5XL5WLFRK4qRbc6ew5hPDNnKJbWJ-D_KESDk-EPFvUd3i6x58F264eEflGxe2Chub8OEUCWxskKSQYObjgf0YLwwi6omDZgLLDhZdNQMgl5sJb4yx2EkxpjXpj-E5Mtk0QQyiJEYoAFnpStBTAPY1ADHuVCxezizlvUsMtVqTW7XIJ07GVoFvqKaajor8_CFBix1wNH9dirlxOcmaIg15eUpsi2glxfOfYjS6kq-qNIL1g6XB-Ifwm8k-0SnD6qeGYVTyWxdSQ8R-a75dbPcjH1xUCMiJxPiXxLhGNRQVuW9TrBa5LnL1FKaSD4aTZXU3ZYBBfkc2kNEygl6_CGc5BioU_WoZisMzNumqyxGbdIVgAW0wUeNclKlORlZ1HDr8nWVvyadFX5pdgnKouxr5hwMPbBVzz202paEWsU6MYJWw9tK9CNM1YHLeWP6JIiCWDpcMoSH-9LWdEPJZ5KYutI1JT9DeQHDZJWjIucM2fUpi2FlHdiTeYSA6vZK-MLKefSoWNP-16Mtm_nkyvb_nVyLQV1wVq6YxsHOMNH4iTOFtmMXBnBiw7DW7yqG5JBheuP48n1vNmMLI_t9Nzh-pkjsgTAIDsbkQ-hP39EPoTexYgseIKhtxqRBVsr_CqFraHgNJoRGVbVEXk6_oj-jAMI9MUeTjIeS07TjccUgqCVSgrDMVH5CcMxRZ06wj5WIDFUSiHvYYIuOdwqibbIA5DXsI9CIQSy-8UBNHlHey6avL81RON4hX38Pbt39ZvdePJuoQCYAkD9lqYBsAWA-o2LApTxBFqXmZbXe1iq6SkBI30UIUwNhPi9rf2-Ol_OEgxzMqbbSZrCRLh_xJTRmAB1xCyRCnBxzuT0nJvKF8TL25xGP-rUIIhIpUD5lA-N4Evk0ql0l7KK8mbnBx5ih7bwz3ZLivf-eVMQUqpbUVAyXBg9BcPlppbTMk7S-wF146aWquD1Ui6h973U-pSYTATRAB41_axgyaKGW9PNtuDW9LIyN-dnynIASCNYa9DJlg4p2zeh3xRMUbAbtLKNsW0Fu0EvewC7RC-8oGQyLNUUi0onK5itAVNpbC1Ndd779St2IVxblg6CJmWQMA9SxGq1GDKGQqRcIjR6KG1w6RDeEecU0BG_x94aOrzx7K3oegZZ5uuTOmKJtfQq64jLSyO5IdbdFiF6k9SkJZ5f__WeXBod6oOBV-50URh54J4jKDPFK0uz-wArdygrPwisF6vVCD7HUK-iO2y9cN1T-BSPvS--l22sQfz1GFDh9byA7xRA3hEyOBc-teRmO3K7IXnZ3knG7aUXXuiTvSFCyh14Bw4WW-4GLhA77FbkdkPyVg6WM-aZDoYgF9OV7f-v-VZLS9Yy2GE15vPOlYofjVx3teLiT0_Pz8FcSbx5KNrZuWx3aOR44Mlo6svsyFChC3imavDW-PYJFTuGd0QKE9RyHKqvqbzEoUKJSC6FoI5Gaqgc9G55hdMtnt0hXn4k3Smc2S1cl8bmh3mdwpndwj3DWLVOkrmwA93YfNgZlN0RFO9nO4Uzu4V79vsUy1cUFz-2gPoV-OtNhnR7Db_kp5Jxf3UGhXNv4PEL-lbkdkNyfgnYitxsR95GGaECNCQ325EfUEYJEXb81ABeOGtqQ203pOYDcytysx15rWPk_qpHxj0p5GM_vKs0Vso1PmvxiJADr4rfJLahp7caezkUqzUHPQ2ECcNijaDcW_kITE6I4sgPYao7usdBEH2peEn4JQeDXQ1x_9DUw697WzGUvaSGodL4l4d_DYQoM3-NFH6ysMGOR6YFfcdXHKNTMG-IPc_Zb2Nx1NiCeNCGeLifWI2txlqXB6QtiAdtiGu1hjfBf_Wa5tdkxdlAvEviQNm-lN-cFhXjXOr9R6PBgD_W78XyFV0XiPIveZ-MaBwb68T3DCtLdvjY2OJk65BH44FIuzWyDd7iW8OCfz28cnZBdmvcho_AFjvh31G0ZZxJtFtvDGsFBRCedrEHJXHqO-vEKUmgHcHJJNqFmWGd5QiG9WB8NSzzbHBiDs7Ny7OBeW6ORufmsXFvWL3R2cnl5ag_PD03L0YXZ_3Lx2Pj31xo_-T89PRiMLroXwwvB2fm6eDYwJ4PNfsD_X12_jPtx_8AiXAdKQ)

The diagram above illustrates the complete workflow with Noise Protocol communication between the coordinator and attestors. Red boxes indicate Noise protocol operations, blue boxes show coordinator commands, green boxes represent attestor responses, and pink boxes denote coordinator-only operations.

The source Mermaid diagram is available at [`flowchart-with-comms.mmd`](flowchart-with-comms.mmd) and can be edited with tools like [Mermaid Live Editor](https://mermaid.live/).

## Table of Contents

1. [Introduction to Noise Protocol](#introduction-to-noise-protocol)
2. [Architecture Overview](#architecture-overview)
3. [Connection Lifecycle](#connection-lifecycle)
4. [Handshake Process](#handshake-process)
5. [Message Types](#message-types)
6. [Message Format](#message-format)
7. [Security Properties](#security-properties)
8. [Implementation Details](#implementation-details)
9. [Error Handling](#error-handling)
10. [Examples](#examples)

---

## Introduction to Noise Protocol

The **Noise Protocol Framework** is a framework for building cryptographic protocols based on Diffie-Hellman key agreement. It provides:

- **Mutual authentication**: Both parties verify each other's identity
- **Forward secrecy**: Past communications remain secure even if long-term keys are compromised
- **Encrypted channels**: All data is encrypted after handshake
- **Minimal overhead**: Efficient and lightweight
- **Pattern flexibility**: Multiple handshake patterns for different security requirements

### Why Noise for This Project?

In the multi-party Canton setup:
- **Security**: Sensitive cryptographic material (keys, signatures) must be transmitted securely
- **Authentication**: Each attestor must verify they're communicating with the legitimate coordinator
- **Privacy**: Topology proposals and signatures should not be visible to eavesdroppers
- **Simplicity**: Noise provides all this with a simple, well-tested framework

---

## Architecture Overview

### Network Topology

```
   Attestor-1 (Coordinator/Server Role)
              |
    +---------+---------+
    |                   |
Attestor-2          Attestor-3+
(Client)            (Client)
```

**Note**: The coordinator is itself an attestor participant - it generates keys, signs proposals, and participates in the decentralized party setup just like other attestors. It additionally takes on the orchestration role by running as a server.

### Role Definitions

**Coordinator (Server + Attestor Role)**:
- **Is an attestor**: Generates keys, signs proposals, participates in threshold signatures
- **Acts as orchestrator**: Listens on a TCP port and accepts connections from other attestors
- Distributes proposals and commands to other attestors
- Aggregates signatures from all attestors (including itself)
- Submits aggregated transactions to Canton
- Can be any of the attestors based on coordinator selection strategy

**Other Attestors (Client Role)**:
- Connect to the coordinator
- Authenticate using their static keys
- Receive commands and data
- Perform operations (signing, key generation)
- Send results back to coordinator
- Participate equally in the decentralized party setup

---

## Connection Lifecycle

### Phase 1: Initialization

```rust
// Coordinator starts listening
coordinator.listen("0.0.0.0:9000");

// Each attestor connects
attestor1.connect("coordinator.example.com:9000");
attestor2.connect("coordinator.example.com:9000");
attestor3.connect("coordinator.example.com:9000");
```

### Phase 2: Handshake

Each attestor performs a Noise handshake with the coordinator:

1. **Key Exchange**: Establish ephemeral session keys
2. **Authentication**: Verify static public keys
3. **Channel Binding**: Create encrypted transport state

### Phase 3: Active Communication

Once all attestors are connected:

1. **Coordinator sends commands** (broadcast or unicast)
2. **Attestors respond** with acknowledgments or data
3. **Coordinator collects responses** and proceeds to next step

### Phase 4: Graceful Shutdown

After setup completion:

1. **Coordinator sends DISCONNECT** command
2. **Attestors acknowledge** and close connections
3. **Coordinator shuts down** listener

---

## Handshake Process

### Noise Pattern: `NN_PSK2`

We use the **Noise NN_PSK2** pattern, which provides:
- Mutual authentication via pre-shared keys (PSK)
- Forward secrecy through ephemeral key exchange
- No identity transmission (identities established through PSK)
- Two round trips

#### How NN_PSK2 Works

Unlike patterns that transmit static keys (like XX), NN_PSK2 uses a **pre-shared key (PSK)** that both parties compute independently from their static keypairs via ECDH. This PSK is mixed into the handshake to provide mutual authentication without transmitting identities.

**PSK Derivation**:
```rust
// Each party has a secp256k1 static keypair
let my_static_keypair = NoiseKeypair::from_file("my_key.priv")?;
let peer_static_pubkey = load_peer_pubkey_from_config();

// PSK is derived via ECDH(my_static_private, peer_static_public)
let psk = my_static_keypair.derive_psk(&peer_static_pubkey)?;
```

#### Handshake Flow

```
Attestor                           Coordinator
   |                                    |
   |  -> e (ephemeral key)              |
   |                                    |
   |  <- e, ee, psk (ephemeral + PSK)   |
   |                                    |
   [Handshake complete, transport mode]
```

#### Step-by-Step

**Pre-handshake**:
- Both parties load their static secp256k1 keypairs
- Both parties load peer's public key from configuration
- Both parties derive the same PSK via ECDH

**Message 1 (Attestor → Coordinator)**:
```
e: Attestor generates ephemeral keypair and sends public key
```

**Message 2 (Coordinator → Attestor)**:
```
e:   Coordinator sends ephemeral public key
ee:  Diffie-Hellman(AttestorEphemeral, CoordinatorEphemeral)
psk: Mix pre-shared key into the handshake state (PSK2 indicates it's mixed at this point)
```

After message 2, both parties have:
- Established ephemeral Diffie-Hellman shared secret
- Mixed in the PSK for mutual authentication
- Derived shared transport encryption keys
- Verified each other's identity (via PSK validation)

### Static Key Management

Each party has a long-term static **secp256k1** keypair:

```rust
// Coordinator generates/loads static key
let coordinator_static_key = NoiseKeypair::from_file("coordinator_key.priv")?;
// Private key: 32 bytes (hex-encoded in file)
// Public key: 33 bytes (compressed secp256k1 format)

// Attestor generates/loads static key
let attestor_static_key = NoiseKeypair::from_file("attestor_key.priv")?;

// Public keys are distributed out-of-band
// (e.g., during initial setup, via configuration files)
```

**Trust Establishment**:
- Attestors know coordinator's static public key (configured in `network.toml`)
- Coordinator maintains allowlist of attestor static public keys (from `network.toml`)
- PSK derivation succeeds only if both parties have correct peer public keys
- Connections from unknown keys are rejected during handshake

---

## Message Types

After handshake, all messages follow a structured protocol.

### Command Messages (Coordinator → Attestor)

| Command | Description | Payload |
|---------|-------------|---------|
| `UPLOAD_DARS` | Instruct attestor to upload DAR files | None |
| `GENERATE_KEYS` | Instruct attestor to generate keys | None |
| `SIGN_DNS` | Instruct attestor to sign DNS proposal | `dns_proto.bin` |
| `SIGN_P2P_PTK` | Instruct attestor to sign P2P proposals (Canton 3.4+: PTK deprecated) | `p2p_proto.bin` |
| `SIGN_SUBMISSIONS` | Instruct attestor to sign ledger submissions | `prepared-submission-*.bin` (3 files) |
| `STATUS_UPDATE` | Inform attestors of progress | Status string |
| `DISCONNECT` | Graceful shutdown | None |

### Response Messages (Attestor → Coordinator)

| Response | Description | Payload |
|----------|-------------|---------|
| `ACK` | Command acknowledged | None |
| `DATA` | Sending requested data | Binary payload |
| `ERROR` | Operation failed | Error message |
| `READY` | Ready for next command | None |
| `WAIT` | Waiting/processing | None |

### Data Transfer Messages

| Message | Direction | Description |
|---------|-----------|-------------|
| `KEYS_UPLOAD` | Attestor → Coordinator | `attestor-public-keys.bin` + `participant-id.bin` |
| `DNS_SIGNATURE` | Attestor → Coordinator | `signed-dns-proposal.bin` |
| `P2P_PTK_SIGNATURES` | Attestor → Coordinator | `signed-p2p-ptk-proposals.bin` (Canton 3.4+: only P2P signatures) |
| `SUBMISSION_SIGNATURES` | Attestor → Coordinator | `submission-signatures.bin` |

---

## Message Format

### Wire Format

All messages after handshake use the following format:

```
+----------------+------------------+--------------------+
| Message Type   | Payload Length   | Payload            |
| (2 bytes)      | (4 bytes)        | (variable)         |
+----------------+------------------+--------------------+
```

- **Message Type**: 16-bit unsigned integer (big-endian)
- **Payload Length**: 32-bit unsigned integer (big-endian)
- **Payload**: Variable-length binary data

### Encryption

All bytes are encrypted using the Noise transport cipher (ChaChaPoly):

```
Encrypted Packet = Encrypt(MessageType || PayloadLength || Payload)
```

### Message Type Enumeration

```rust
pub enum MessageType {
    // Commands (0x0001 - 0x0007)
    UploadDars = 0x0001,
    GenerateKeys = 0x0002,
    SignDns = 0x0003,
    SignP2pPtk = 0x0004,
    SignSubmissions = 0x0005,
    StatusUpdate = 0x0006,
    Disconnect = 0x0007,

    // Responses (0x0101 - 0x0105)
    Ack = 0x0101,
    Data = 0x0102,
    Error = 0x0103,
    Ready = 0x0104,
    Wait = 0x0105,

    // Data Transfers (0x0201 - 0x0204)
    KeysUpload = 0x0201,
    DnsSignature = 0x0202,
    P2pPtkSignatures = 0x0203,
    SubmissionSignatures = 0x0204,
}
```

### Example Message Encoding

**Command: UPLOAD_DARS**
```
[0x00, 0x01] [0x00, 0x00, 0x00, 0x00]
  ^^ type      ^^ length (0 bytes)
```

**Response: DATA with 1024 bytes**
```
[0x01, 0x02] [0x00, 0x00, 0x04, 0x00] [... 1024 bytes ...]
  ^^ type      ^^ length (1024)         ^^ payload
```

---

## Security Properties

### Confidentiality

- All data after handshake is encrypted with ChaChaPoly-1305
- Eavesdroppers cannot read message contents
- Includes keys, signatures, proposals, and commands

### Authentication

- Static keys ensure both parties are who they claim to be
- Coordinator only accepts connections from known attestors
- Attestors verify coordinator's identity before proceeding

### Integrity

- AEAD cipher (ChaChaPoly) provides authentication
- Any tampering results in decryption failure
- Messages cannot be modified in transit

### Forward Secrecy

- Ephemeral keys used in handshake
- If long-term keys compromised, past sessions remain secure
- New ephemeral keys for each connection

### Replay Protection

- Noise protocol includes nonce progression
- Old messages cannot be replayed
- Session keys are unique per connection

### Protection Against

| Attack | Protection |
|--------|------------|
| Eavesdropping | Full encryption |
| Man-in-the-Middle | Mutual authentication |
| Replay | Nonce progression |
| Tampering | AEAD authentication |
| Impersonation | Static key verification |
| Connection hijacking | Session binding |

---

## Implementation Details

### Actual Implementation

This project uses **`tokio-noise`** and **`hyper-noise`** libraries for Noise Protocol integration with async Tokio and HTTP/Hyper infrastructure.

**Libraries Used:**
- **`tokio-noise`** - Provides async Noise protocol integration with Tokio
- **`hyper-noise`** - Enables HTTP over Noise transport
- **`hyper`** - HTTP server/client infrastructure
- **`secp256k1`** - Elliptic curve cryptography for static keypairs

### Architecture

**Core Components:**

```
NoiseServer (Coordinator)
├─ Hyper HTTP server
├─ tokio-noise integration for Noise NN_PSK2
├─ secp256k1 static keypair
└─ Peer allowlist (from network.toml)

NoiseClient (Attestor)
├─ Hyper HTTP client
├─ tokio-noise for encrypted transport
├─ secp256k1 static keypair
└─ Coordinator peer public key
```

### Key Structures and Functions

**NoiseKeypair** (`src/noise/mod.rs`):
```rust
pub struct NoiseKeypair {
    private_key: SecretKey,        // 32-byte secp256k1 private key
    public_key: PublicKey,         // 33-byte compressed public key
}

impl NoiseKeypair {
    // Generate new random keypair
    pub fn generate() -> Self;

    // Load from file (hex-encoded private key)
    pub fn from_file(path: &Path) -> Result<Self>;

    // Derive PSK via ECDH with peer's public key
    pub fn derive_psk(&self, peer_public_key: &PublicKey) -> Result<[u8; 32]>;

    // Get public key bytes (33 bytes compressed)
    pub fn public_key_bytes(&self) -> [u8; 33];
}
```

**NoiseServer** (`src/noise/server.rs`):
```rust
pub struct NoiseServer {
    static_keypair: NoiseKeypair,
    allowed_peers: HashSet<PublicKey>,  // From network.toml
    listener_addr: SocketAddr,
}

impl NoiseServer {
    pub fn new(
        static_keypair: NoiseKeypair,
        allowed_peers: Vec<PublicKey>,
        listener_addr: SocketAddr,
    ) -> Self;

    // Start listening and accepting connections
    pub async fn listen(&self) -> Result<()>;

    // Accept and handshake with a client
    async fn accept_connection(&self) -> Result<(NoiseStream, PublicKey)>;
}
```

**NoiseClient** (`src/noise/client.rs`):
```rust
pub struct NoiseClient {
    static_keypair: NoiseKeypair,
    coordinator_pubkey: PublicKey,
    connection: Option<NoiseStream>,
}

impl NoiseClient {
    pub fn new(
        static_keypair: NoiseKeypair,
        coordinator_pubkey: PublicKey,
    ) -> Self;

    // Connect to coordinator and perform handshake
    pub async fn connect(&mut self, addr: &str) -> Result<()>;

    // Send a message over encrypted channel
    pub async fn send_message(&mut self, msg: Message) -> Result<()>;

    // Receive next message
    pub async fn receive_message(&mut self) -> Result<Message>;

    // High-level data transfer methods
    pub async fn upload_keys(&mut self, ...) -> Result<()>;
    pub async fn send_dns_signature(&mut self, ...) -> Result<()>;
    pub async fn send_p2p_ptk_signatures(&mut self, ...) -> Result<()>;
    pub async fn send_submission_signatures(&mut self, ...) -> Result<()>;
}
```

### Handshake Implementation Flow

**Coordinator (Responder)**:
```
1. Load secp256k1 static keypair
2. Derive PSK with each allowed peer's public key
3. Start Hyper server with hyper-noise integration
4. For each incoming connection:
   a. Perform NN_PSK2 handshake as responder
   b. Handshake validates peer via PSK
   c. If successful, connection enters transport mode
   d. Verify peer's public key is in allowlist
   e. Accept connection and start message processing
```

**Attestor (Initiator)**:
```
1. Load secp256k1 static keypair
2. Load coordinator's public key from network.toml
3. Derive PSK with coordinator's public key
4. Connect to coordinator via HTTP/Noise
5. Perform NN_PSK2 handshake as initiator:
   a. Send ephemeral public key
   b. Receive ephemeral key + PSK mixing
   c. Handshake validates coordinator via PSK
6. Enter transport mode (encrypted channel established)
7. Begin polling for commands
```

### Message Protocol

**Message Structure** (`src/noise/mod.rs`):
```rust
pub struct Message {
    pub message_type: MessageType,
    pub payload: Vec<u8>,
}

impl Message {
    // Encode to wire format
    pub fn to_bytes(&self) -> Vec<u8> {
        // [MessageType (2 bytes)] [PayloadLength (4 bytes)] [Payload]
    }

    // Decode from wire format
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        // Parse header and payload
    }
}
```

**Sending/Receiving**:
- Messages are serialized to wire format (type + length + payload)
- Wire format bytes are encrypted by Noise transport (ChaChaPoly-1305)
- Encrypted packets are sent over the underlying TCP/HTTP connection
- `tokio-noise` and `hyper-noise` handle encryption/decryption transparently

### Key Generation Command

Users generate Noise keypairs via CLI:
```bash
cargo run -- keygen -o keys/my-node.key
```

This generates:
- Private key: 32 bytes (saved hex-encoded to file)
- Public key: 33 bytes compressed secp256k1 (printed to share with peers)

### Security Properties

**Authentication**:
- PSK derivation via ECDH ensures only parties with correct keypairs can connect
- No static keys transmitted over network (identity hiding)
- Coordinator verifies each attestor's public key against allowlist

**Encryption**:
- All messages encrypted with ChaChaPoly-1305 AEAD cipher
- Handshake establishes session keys from ephemeral DH + PSK

**Forward Secrecy**:
- Ephemeral keys used in each connection
- Session keys independent of static keys
- Past sessions remain secure if static keys later compromised

**Key Points:**
- NN_PSK2 pattern provides mutual authentication without identity transmission
- secp256k1 used for static keypairs (compatible with Canton's key infrastructure)
- PSK derived via ECDH ensures both parties have correct peer public keys
- `tokio-noise` + `hyper-noise` provide high-level async integration
- Message protocol implemented on top of Noise transport layer

### Configuration

Instead of separate coordinator and attestor configurations, all parties use a **shared configuration file** that defines the entire network topology. Each member runs the same program, and the program determines their role based on their identity in the config.

**Shared Network Configuration (`network.toml`)**:
```toml
# Network-wide configuration shared by all participants
[network]
name = "cbtc-setup-network"
protocol_version = "1.0"
port = 9000  # Default port for all nodes

# Coordinator selection strategy
# Options: "first", "explicit", "election"
coordinator_strategy = "explicit"

# List of all participants in the setup
# Order matters if coordinator_strategy = "first"
[[participants]]
id = "coordinator-node"
name = "Coordinator Node"
role = "coordinator"  # Only used if coordinator_strategy = "explicit"
address = "10.0.1.100"
port = 9000
public_key = "02c0011d1a70123456789abcdef0123456789abcdef0123456789abcdef012345"  # 33-byte secp256k1 public key (hex)

[[participants]]
id = "attestor-1"
name = "Attestor 1"
address = "10.0.1.101"
port = 9000
public_key = "03a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456"

[[participants]]
id = "attestor-2"
name = "Attestor 2"
address = "10.0.1.102"
port = 9000
public_key = "02f6e5d4c3b2a1098765432109876543210fedcba098765432109876543210fed"

[[participants]]
id = "attestor-3"
name = "Attestor 3"
address = "10.0.1.103"
port = 9000
public_key = "031a2b3c4d5e6f098765432109876543210abcdef1234567890abcdef123456789"

[timeouts]
handshake_timeout_secs = 30
message_timeout_secs = 120
connection_retry_attempts = 3
connection_retry_delay_secs = 5

# Application-specific configuration (required)
[application]
party_id_prefix = "cbtc-network"
namespace_key_name = "cbtc-network-namespace"
daml_key_name = "cbtc-network-daml-transactions"
operator_party_hint = "operator"

# Contract definitions (optional)
# [[application.contracts]]
# id = "create-govR"
# name = "CBTCGovernanceRules"
# package_id = "#cbtc-governance"
# module_name = "CBTC.Governance"
# entity_name = "CBTCGovernanceRules"
# fields = [...]
```

**Individual Node Configuration (`node.toml`)**:
```toml
# Each participant has their own node configuration
# This identifies who they are in the network

# Path to shared network configuration
network_config = "network.toml"

[node]
# Must match one of the IDs in network.toml
node_id = "attestor-1"

# Path to this node's static private key
static_key_file = "keys/attestor-1_static.key"

# Override listen address (default: 0.0.0.0)
listen_address = "0.0.0.0"

[canton]
# Canton participant configuration
admin_api_host = "localhost"
admin_api_port = 5001
ledger_api_host = "localhost"
ledger_api_port = 5002
# Synchronizer name (default: "global")
synchronizer = "global"
# Ledger API user ID for submission operations (required)
ledger_api_user_id = "ledger-api-user"
# Optional: JWT token for Ledger API authentication
# ledger_api_token = "your-jwt-token-here"
```

### Coordinator Selection Strategies

The configuration supports three strategies for determining which node acts as the coordinator:

#### 1. Explicit Selection (`coordinator_strategy = "explicit"`)

**Recommended for production**. One participant is explicitly designated as coordinator in the network configuration:

```toml
[[participants]]
id = "coordinator-node"
role = "coordinator"  # Explicitly designated
```

**Advantages**:
- Clear, deterministic selection
- No ambiguity about roles
- Easy to troubleshoot

**Disadvantages**:
- Single point of failure (if coordinator node is down, setup cannot proceed)
- Requires coordination to decide who is coordinator before starting

#### 2. First Node Selection (`coordinator_strategy = "first"`)

The first participant listed in the configuration acts as coordinator:

```toml
coordinator_strategy = "first"

# The first participant becomes coordinator
[[participants]]
id = "node-1"  # This one will be coordinator
# ...

[[participants]]
id = "node-2"  # This and others are attestors
# ...
```

**Advantages**:
- Simple, deterministic
- No explicit role assignment needed

**Disadvantages**:
- Still a single point of failure
- Somewhat arbitrary

#### 3. Leader Election (`coordinator_strategy = "election"`)

**Most robust**. Nodes perform a distributed leader election if the designated coordinator is unavailable.

**Election Algorithm**:
1. All nodes attempt to connect to the designated coordinator
2. If coordinator doesn't respond within timeout, trigger election
3. Election uses **Bully Algorithm**:
   - Node with highest ID (lexicographically) becomes coordinator
   - If that node is down, next highest takes over
4. Once elected, new coordinator announces its role to all peers
5. All nodes update their local state

```
Election Algorithm (Bully Pattern):

1. Try connecting to designated coordinator first
   - If reachable → Use designated coordinator
   - If unreachable → Proceed to election

2. Sort all participant IDs lexicographically

3. Starting from highest ID, iterate downwards:
   a. If current candidate is me:
      → I am the highest available ID
      → Declare myself coordinator
      → Announce to other peers
      → Break

   b. Try connecting to candidate:
      → If reachable: Accept them as coordinator, break
      → If unreachable: Continue to next lower ID

4. If no coordinator found → Error (insufficient quorum)

Result: Deterministic coordinator selection
- Always the same coordinator given same availability
- No split-brain (highest available ID always wins)
- Automatic failover if coordinator goes down
```

**Advantages**:
- Fault-tolerant
- Automatic failover if coordinator goes down
- More resilient to network issues

**Disadvantages**:
- More complex implementation
- Potential for split-brain scenarios (mitigated by using deterministic election)

### Program Startup Flow

Each participant runs the same program binary with their own `node.toml`:

```bash
# On node 1 (10.0.1.100) - Run onboarding workflow
$ cargo run -- -c node.toml onboarding

# On node 2 (10.0.1.101)
$ cargo run -- -c node.toml onboarding

# On node 3 (10.0.1.102)
$ cargo run -- -c node.toml onboarding

# On node 4 (10.0.1.103)
$ cargo run -- -c node.toml onboarding
```

**Startup Sequence:**

```
Startup Flow:
1. Load node.toml (my identity + Canton config)
2. Load network.toml (shared topology + all public keys)
3. Determine coordinator:
   - Explicit: Read from network.toml role field
   - First: Take first entry in participants list
   - Election: Run Bully algorithm to elect highest available ID
4. Determine my role (am I the coordinator?)
5. Branch:
   - If coordinator: Start listening, wait for attestors
   - If attestor: Connect to coordinator
6. Begin workflow execution (onboarding or contracts)
```

### Initial Setup: Key Generation and Exchange

Before the distributed setup can begin, participants must perform an initial bootstrapping process to establish trust.

#### Step 1: Generate Static Keypair

Each participant generates their own Noise static keypair (secp256k1):

```bash
# Each participant runs this command
$ cargo run -- keygen -o keys/my_static.key

Generating secp256k1 static keypair...
Private key saved to: keys/my_static.key (32 bytes, hex-encoded)
Public key (hex): 02a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456

⚠️  Keep your private key secure! Never share it with anyone.
💡  Share your public key (33 bytes compressed) with other participants to add to network.toml
```

The private key is stored securely (hex-encoded in the file), and the public key (33-byte compressed secp256k1 format, hex-encoded) is displayed for sharing with other participants.

#### Step 2: Exchange Public Keys

Participants exchange their public keys through a **secure out-of-band channel**:

- **In-person meeting**: Exchange keys via USB drive or QR code
- **Secure messaging**: Use Signal, PGP-encrypted email, etc.
- **Video call**: Read keys aloud and verify (for small groups)
- **Blockchain**: Post commitments to public keys on-chain (for trustless setup)

**Example Exchange (3 participants)**:

```
Alice: My public key is: 02a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456
Bob:   My public key is: 03f6e5d4c3b2a1098765432109876543210fedcba098765432109876543210fed
Carol: My public key is: 021a2b3c4d5e6f098765432109876543210abcdef1234567890abcdef123456789

(Note: Public keys are 33 bytes compressed secp256k1 format, starting with 02 or 03)

Alice: I propose to be the coordinator since I have the most stable connection.
Bob:   Agreed.
Carol: Agreed. Alice will be coordinator.

Alice: My IP address is 10.0.1.100
Bob:   My IP address is 10.0.1.101
Carol: My IP address is 10.0.1.102
```

#### Step 3: Create Shared Network Configuration

One participant (typically the designated coordinator) creates the `network.toml` file:

```toml
[network]
name = "cbtc-setup-network"
protocol_version = "1.0"
port = 9000
coordinator_strategy = "explicit"

[[participants]]
id = "alice"
name = "Alice (Coordinator)"
role = "coordinator"
address = "10.0.1.100"
port = 9000
public_key = "02a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456"

[[participants]]
id = "bob"
name = "Bob"
address = "10.0.1.101"
port = 9000
public_key = "03f6e5d4c3b2a1098765432109876543210fedcba098765432109876543210fed"

[[participants]]
id = "carol"
name = "Carol"
address = "10.0.1.102"
port = 9000
public_key = "021a2b3c4d5e6f098765432109876543210abcdef1234567890abcdef123456789"

[timeouts]
handshake_timeout_secs = 30
message_timeout_secs = 120
connection_retry_attempts = 3
connection_retry_delay_secs = 5

[application]
party_id_prefix = "cbtc-network"
namespace_key_name = "cbtc-network-namespace"
daml_key_name = "cbtc-network-daml-transactions"
operator_party_hint = "operator"
```

#### Step 4: Distribute Network Configuration

The `network.toml` file is distributed to all participants through the same secure channel:

```bash
# Coordinator creates and signs the network config
$ sha256sum network.toml
8f43e... network.toml

# Coordinator shares file and hash with all participants
# Each participant verifies the hash matches
$ sha256sum network.toml
8f43e... network.toml  ✓ Hash matches!
```

#### Step 5: Create Individual Node Configuration

Each participant creates their own `node.toml`:

```bash
# Alice (coordinator)
$ cat node.toml
network_config = "network.toml"

[node]
node_id = "alice"
static_key_file = "keys/my_static.key"
listen_address = "0.0.0.0"

[canton]
admin_api_host = "localhost"
admin_api_port = 5001
ledger_api_host = "localhost"
ledger_api_port = 5002
synchronizer = "global"
ledger_api_user_id = "ledger-api-user"
# ledger_api_token = "your-jwt-token-here"

# Bob
$ cat node.toml
network_config = "network.toml"

[node]
node_id = "bob"
static_key_file = "keys/my_static.key"
listen_address = "0.0.0.0"

[canton]
admin_api_host = "localhost"
admin_api_port = 5011
ledger_api_host = "localhost"
ledger_api_port = 5012
synchronizer = "global"
ledger_api_user_id = "ledger-api-user"
# ledger_api_token = "your-jwt-token-here"

# Carol - similar configuration
```

Now all participants are ready to run the distributed setup!

### Distributed Setup Example

**Prerequisites** (completed above):
1. ✅ All participants have agreed on the network topology
2. ✅ Each participant has generated their static Noise keypair
3. ✅ Public keys have been shared and added to `network.toml`
4. ✅ `network.toml` is distributed to all participants
5. ✅ Each participant has created their `node.toml`

**Execution**:

```bash
# Participant 1 (Coordinator - 10.0.1.100)
$ cat node.toml
[node]
node_id = "coordinator-node"
static_key_file = "keys/coordinator_static.key"
network_config = "network.toml"

$ cargo run -- -c node.toml onboarding
[INFO] I am: coordinator-node
[INFO] Coordinator: coordinator-node
[INFO] My role: COORDINATOR
[INFO] Starting as COORDINATOR
[INFO] Listening on 0.0.0.0:9000
[INFO] Expecting 3 attestors to connect
[INFO] Attestor connected: attestor-1 (10.0.1.101)
[INFO] Attestor connected: attestor-2 (10.0.1.102)
[INFO] Attestor connected: attestor-3 (10.0.1.103)
[INFO] All attestors connected, starting onboarding workflow
[INFO] Broadcasting command: GENERATE_KEYS
...

# Participant 2 (Attestor 1 - 10.0.1.101)
$ cat node.toml
[node]
node_id = "attestor-1"
static_key_file = "keys/attestor-1_static.key"
network_config = "network.toml"

$ cargo run -- -c node.toml onboarding
[INFO] I am: attestor-1
[INFO] Coordinator: coordinator-node
[INFO] My role: ATTESTOR
[INFO] Starting as ATTESTOR
[INFO] Connecting to coordinator at 10.0.1.100:9000
[INFO] Connected to coordinator, ready for commands
[DEBUG] Received command: GENERATE_KEYS
[INFO] Generating cryptographic keys...
[INFO] Keys generated, sending to coordinator
...

# Participant 3 (Attestor 2 - 10.0.1.102) and 4 (Attestor 3 - 10.0.1.103)
# Similar output to Participant 2
```

---

## Error Handling

### Connection Errors

| Error | Cause | Handling |
|-------|-------|----------|
| `HandshakeFailed` | Authentication failure | Reject connection, log |
| `UnknownPeer` | Attestor not in allowlist | Reject connection, log |
| `Timeout` | Network or peer unresponsive | Retry or abort |
| `DecryptionError` | Message tampered or corrupted | Close connection |
| `InvalidMessage` | Malformed message | Send ERROR response |

### Protocol Errors

```
Error Categories:

1. Connection Errors:
   - HandshakeFailed: Noise handshake didn't complete
   - UnknownPeer: Remote static key not in allowlist
   - ConnectionClosed: TCP connection dropped
   - Timeout: Peer not responding within deadline

2. Protocol Errors:
   - DecryptionError: Message authentication failed (tampering detected)
   - InvalidMessage: Malformed message structure
   - ProtocolViolation: Unexpected message type or sequence

3. Application Errors:
   - CantonError: Canton gRPC operation failed
   - FileNotFound: Missing keys/proposals/signatures
   - SignatureInvalid: Cryptographic signature verification failed
```

### Retry Strategy

```
Exponential Backoff with Jitter:

For transient errors (network, timeout):
  attempt = 1
  while attempt <= max_retries:
    try operation
    if success:
      return success

    delay = 2^attempt + random(0, 1) seconds
    log warning: "Attempt {attempt}/{max_retries} failed, retrying in {delay}s"
    sleep(delay)
    attempt++

  return error (exhausted retries)

For permanent errors (authentication, protocol):
  → Fail immediately, no retry
  → Log error and close connection
```

---

## Communication Patterns

### Pattern 1: Command Broadcast with Acknowledgments

```
Coordinator                          Attestor 1, 2, 3
    |                                      |
    |--- UPLOAD_DARS (broadcast) -------->|
    |                                      | (perform upload)
    |<-------- ACK ------------------------|
    |                                      |
    | (wait for all ACKs with timeout)    |
    |                                      |
    |--- GENERATE_KEYS ------------------>|
    |                                      | (generate keys)
    |<-------- DATA (keys) ---------------|
    |                                      |
    | (collect all keys)                  |
```

**Flow:**
1. Coordinator broadcasts command to all attestors
2. Each attestor processes independently
3. Each attestor sends response (ACK or DATA)
4. Coordinator waits for all responses with timeout
5. If any timeout → retry or abort
6. Once all collected → proceed to next step

### Pattern 2: File Distribution

```
Coordinator                          Attestor
    |                                   |
    |--- FILE_METADATA (size) -------->|
    |--- FILE_CHUNK (1) -------------->|
    |--- FILE_CHUNK (2) -------------->|
    |--- FILE_CHUNK (N) -------------->|
    |                                   | (reassemble file)
    |<-------- ACK ---------------------|
```

**Flow:**
1. Coordinator sends metadata (total size, chunks)
2. Coordinator sends file in 64KB chunks
3. Attestor receives and buffers all chunks
4. Attestor verifies completeness
5. Attestor sends ACK when complete

### Pattern 3: Signature Collection

```
Coordinator                          Attestor 1, 2, 3
    |                                      |
    |--- PROPOSAL_DATA ------------------>|
    |                                      | (sign proposal)
    |<-------- SIGNATURE -----------------|
    |                                      |
    | (wait for threshold signatures)     |
    |                                      |
    | (aggregate signatures)               |
    | (submit to Canton)                   |
    |                                      |
    |--- STATUS_UPDATE (success) -------->|
```

**Flow:**
1. Coordinator distributes proposal to sign
2. Each attestor signs independently with their key
3. Each attestor sends signature back
4. Coordinator collects signatures until threshold met
5. Coordinator aggregates all signatures
6. Coordinator submits to Canton
7. Coordinator broadcasts success status

### Pattern 4: Graceful Shutdown

```
Coordinator                          Attestor
    |                                   |
    |--- DISCONNECT ------------------>|
    |                                   | (cleanup resources)
    |<-------- ACK ---------------------|
    | (close connection)                | (close connection)
```

---

## Summary

The Noise Protocol Framework provides a secure, authenticated communication channel between the coordinator and attestors during the multi-party Canton setup. Key benefits:

- ✅ **Strong security**: Mutual authentication via PSK, encryption, forward secrecy
- ✅ **Simple implementation**: Well-defined protocol with `tokio-noise` and `hyper-noise` libraries
- ✅ **Low overhead**: Minimal performance impact with ChaChaPoly-1305 cipher
- ✅ **Proven design**: Noise framework used in WireGuard, Lightning Network, and other systems
- ✅ **Identity hiding**: NN_PSK2 pattern doesn't transmit static keys over the network
- ✅ **Flexible**: Supports various network topologies and message patterns
- ✅ **Distributed setup**: Each member runs the same program independently
- ✅ **No central server**: Coordinator is just another peer with an orchestration role
- ✅ **Configurable coordination**: Multiple strategies for coordinator selection
- ✅ **secp256k1 compatibility**: Uses same curve as Canton's cryptographic infrastructure

The protocol ensures that sensitive cryptographic material (keys, signatures, proposals) is transmitted securely between peers, and that all parties are who they claim to be through PSK-based mutual authentication.

### Key Design Decisions

**1. Shared Configuration File**
- All participants use the same `network.toml`
- Contains complete network topology and all public keys
- Distributed via secure out-of-band channel
- Ensures everyone has consistent view of the network

**2. Individual Node Configuration**
- Each participant has their own `node.toml`
- Identifies which participant they are in the network
- Contains their private key path and Canton connection details
- Allows same binary to run in different roles

**3. Coordinator Selection**
- Three strategies: explicit, first-node, or election
- Explicit selection recommended for production (clear, deterministic)
- Election provides fault tolerance but adds complexity
- Coordinator is peer-elected, not a privileged central server

**4. Single Binary, Multiple Roles**
- Same program binary runs on all nodes
- Role (coordinator vs attestor) determined at runtime from configuration
- Reduces deployment complexity and ensures consistency
- Each node can potentially be coordinator (with election strategy)

**5. Zero Trust Model**
- All connections authenticated via Noise NN_PSK2 with PSK derived from static secp256k1 keys
- No implicit trust based on IP addresses
- No static keys transmitted over the network (identity hiding)
- Public keys exchanged out-of-band before setup
- Unknown peers rejected immediately during handshake

### Deployment Workflow Summary

```
1. Initial Setup (One-time, Out-of-Band)
   ├─ Each participant generates Noise keypair: cargo run -- keygen -o keys/my.key
   ├─ Public keys exchanged securely (Signal, in-person, etc.)
   ├─ Coordinator creates network.toml with all participants
   ├─ network.toml distributed to all participants
   └─ Each participant creates their node.toml

2. Onboarding Workflow (Run once to create decentralized party)
   ├─ All participants start: cargo run -- -c node.toml onboarding
   ├─ Program reads configs and determines role
   ├─ Coordinator starts listening, attestors connect
   ├─ Mutual authentication via Noise handshake
   ├─ Workflow: GenerateKeys → CreateProposals → SignDns → SubmitDns → SignP2p → SubmitFinal
   └─ Decentralized party namespace created

3. Contracts Workflow (Run after onboarding to deploy contracts)
   ├─ All participants start: cargo run -- -c node.toml contracts
   ├─ Coordinator orchestrates workflow
   ├─ Workflow: UploadDars → PrepareSubmissions → SignSubmissions → ExecuteSubmissions
   └─ Governance contracts deployed to ledger

4. Shutdown
   ├─ Coordinator sends DISCONNECT command
   ├─ All connections gracefully closed
   └─ Session complete
```

This architecture provides a secure, decentralized approach to the multi-party Canton setup while maintaining simplicity for participants. Each member runs one program with one command, and the Noise protocol handles all the security complexity transparently.

---

## References

### Noise Protocol
- [Noise Protocol Framework Specification](https://noiseprotocol.org/noise.html) - Official spec
- [Noise Explorer - Protocol Analysis Tool](https://noiseexplorer.com/) - Security analysis
- [Noise Pattern: NN_PSK2](https://noiseexplorer.com/patterns/NNpsk2/) - Specific pattern used in this project

### Rust Implementations Used in This Project
- [tokio-noise](https://crates.io/crates/tokio-noise) - Tokio async integration with Noise protocol (**used in this project**)
- [hyper-noise](https://crates.io/crates/hyper-noise) - HTTP over Noise integration (**used in this project**)
- [secp256k1](https://crates.io/crates/secp256k1) - Elliptic curve cryptography for static keypairs (**used in this project**)

### Other Rust Noise Implementations
- [snow](https://github.com/mcginty/snow) - Low-level Noise protocol implementation
- [snowstorm](https://crates.io/crates/snowstorm) - Async streams/packets with Noise

### Real-world Usage
- [WireGuard Protocol](https://www.wireguard.com/protocol/) - VPN using Noise IK pattern
- [Lightning Network Bolt #8](https://github.com/lightning/bolts/blob/master/08-transport.md) - Bitcoin Lightning uses Noise XK pattern
