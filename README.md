# Canton gRPC Rust Test

## List services

### Admin API

Command

```sh
grpcurl -plaintext localhost:5002 list
```

Response

```
com.digitalasset.canton.admin.health.v30.StatusService
com.digitalasset.canton.admin.sequencer.v30.SequencerStatusService
com.digitalasset.canton.connection.v30.ApiInfoService
com.digitalasset.canton.crypto.admin.v30.VaultService
com.digitalasset.canton.sequencer.admin.v30.SequencerAdministrationService
com.digitalasset.canton.sequencer.admin.v30.SequencerPruningAdministrationService
com.digitalasset.canton.topology.admin.v30.IdentityInitializationService
com.digitalasset.canton.topology.admin.v30.TopologyAggregationService
com.digitalasset.canton.topology.admin.v30.TopologyManagerReadService
com.digitalasset.canton.topology.admin.v30.TopologyManagerWriteService
grpc.reflection.v1alpha.ServerReflection
```

### Ledger API

Command

```sh
grpcurl -plaintext localhost:5001 list
```

Response

```
com.digitalasset.canton.connection.v30.ApiInfoService
com.digitalasset.canton.sequencer.api.v30.SequencerAuthenticationService
com.digitalasset.canton.sequencer.api.v30.SequencerConnectService
com.digitalasset.canton.sequencer.api.v30.SequencerService
grpc.health.v1.Health
grpc.reflection.v1alpha.ServerReflection
```

## Clone Canton APIs

```sh
mkdir -p proto/canton
git clone git@github.com:hyperledger-labs/splice.git
cp -r ../splice/canton/community/ledger-api/src/main/protobuf proto/canton
cp -r ../splice/canton/community/admin-api/src/main/protobuf proto/canton
```

## Clone Google APIs

```sh
mkdir -p proto/googleapis
git clone https://github.com/googleapis/googleapis.git proto/googleapis
```

## Run The App

```sh
cargo run --release -- \
    --host-url http://localhost \
    --ledger-api-port 5001 \
    --admin-api-port 5002
```
