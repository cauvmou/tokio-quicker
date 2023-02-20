/*
 1. On creation generate a new ConnectionID seed.
 2. For each incoming connection check if the data contains a QUIC-Header and extract the ConnectionID.
    Check if we already have this ConnectionID in our CLIENT-MAP and continue, else verify that the packet is a initial and check the version (Version negotiation).
    Then we get the Token from the header, if this is empty we initiate a retry. For this we need to mint a retry-token and send it to the client.
    But if the token is something we need to verify it.
 3. If all checks out we add the connection to our CLIENT-MAP and use the ConnectionID as a key
*/