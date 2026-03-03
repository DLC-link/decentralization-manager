1. Create a new decentralized party
2. Add dec party vault config section to deployment config then restart deployments
3. Grant readAs and actAs rights for the following member parties (Yaak)

- attestor-1::1220fa8543db6c66fe3a55b1f180c8dfc7f876265c76684fbc1d35d89e02c8aafe8e
- attestor-2::122099953934d9fe163fed07dd371fa13982b2b30749d6df56ecdba385f8c78a867a
- attestor-3::1220d544125d3619e9c4d7340466a78a85f738e75a740837fa6283a1051494f5df23

4. Upload the DARs
5. Deploy VaultGovernance contract

- set package ID to #bitsafe-vault-v0-rc8
- specify the member parties of the participants (hit enter after each party)

6. Create Provider Service
7. Create User Service
8. Setup Utility

- Operator party?

9. Request Devnet FAR
11. Add VaultManager (Yaak)
12. Deploy Vault

- Add the same member parties as beneficiaries

13. Deploy YieldEpoch
14. Request Processor Deployment

- Same beneficiaries as before

15. Accept Processor (Yaak)
16. Accept Free Credential
