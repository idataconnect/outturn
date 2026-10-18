# Amazon Bedrock

Claude reached through Amazon Bedrock, for an operator running on AWS or one
whose customers want their model traffic to stay inside it. The model behaves
the same through either door: the gateway sends Bedrock the same Anthropic
body and reads the same events back (`crates/gateway/src/llm/provider/bedrock.rs`),
and the catalog knows both as one model, so a fallback between them is not a
change of model ([models.md](models.md)).

**Not yet tried against AWS itself.** Everything here is tested against
stand-ins -- MiniStack for the protocol, AWS's published test vectors for the
signing, fakes for STS and the pod identity agent -- and nobody has yet made
a real call to Bedrock with real credentials. The two-doors recipe says so
until somebody has.

## Setting it up

Platform → Setup offers **Claude, two doors**: Claude Sonnet on Bedrock,
falling back to the same model at Anthropic. It asks for the region, which
gives the account its address (`https://bedrock-runtime.<region>.amazonaws.com`)
and picks the cross-region inference profile recent Claude models are called
through: `us.` for a US region, `eu.` for a European one, `global.` otherwise.
An account and model can equally be added by hand on Platform → Models.

## Credentials

Bedrock is reached with one of these, in this order:

1. **A Bedrock API key** on the account, sealed like any provider key. The
   simplest, and the only one that is not the gateway's own.
2. **Keys in the gateway's environment**: `AWS_ACCESS_KEY_ID`,
   `AWS_SECRET_ACCESS_KEY`, and `AWS_SESSION_TOKEN` where they are temporary.
   Fixed for the life of the process, so for local work rather than a cluster.
3. **A role for the gateway's service account** (IAM Roles for Service
   Accounts). Annotate the gateway's service account with the role, and EKS
   gives the pod `AWS_ROLE_ARN` and a token file, which the gateway exchanges
   with STS for temporary keys (`AssumeRoleWithWebIdentity`).
4. **The pod's identity** (EKS Pod Identity). Associate the role with the
   gateway's service account, and the node's agent hands the pod temporary
   keys for the token it is given.

The last two keep nothing long-lived anywhere, which is what a gateway on AWS
should use. Their keys are held until five minutes before they expire and
asked for again, and the token file is read afresh each time, since it
rotates (`crates/gateway/src/llm/provider/aws_credentials.rs`). One identity
for the whole gateway, however many Bedrock accounts it has.

The role needs to invoke models, through profiles and directly:

```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Action": ["bedrock:InvokeModel", "bedrock:InvokeModelWithResponseStream"],
    "Resource": [
      "arn:aws:bedrock:*:123456789012:inference-profile/*",
      "arn:aws:bedrock:*::foundation-model/anthropic.*"
    ]
  }]
}
```

with the account number your own, and the models enabled for the account in
the Bedrock console.

`AWS_REGION` names the region STS is asked in, and the region a request is
signed for where the account's address does not carry one (an emulator's);
`AWS_ENDPOINT_URL_STS` points the role exchange elsewhere, as for a test.

## Trying it without AWS

`scripts/dev.sh --with ministack` runs MiniStack, a local emulator whose
Bedrock runtime streams in AWS's event-stream format, with made-up keys for
the gateway to sign with. Point an account at it and a turn goes through a
Bedrock-shaped door to the local model ([models.md](models.md), phase 8).
