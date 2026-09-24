/**
 * @file pairing_response.h
 * @brief Pairing response classification (decided in Rust, `pairing_response.rs`).
 *
 * The C++ side keeps HTTP, the response buffer, cJSON parsing, the NVS token
 * write and the screen. It passes only facts — status plus what cJSON already
 * established — and gets back the `rf_pairing_outcome_t` the pairing loop feeds
 * into `rf_pairing_decide`. The rules are a compatibility port of the old
 * inline checks in `server_pairing.cc`; they are not tightened.
 */
#ifndef PAIRING_RESPONSE_H
#define PAIRING_RESPONSE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Facts from a pair-start response: HTTP status + what cJSON established.
 *
 * Layout contract with `pairing_response.rs` (`#[repr(C)]` + a Rust test
 * asserting offsets/size): `int32_t status` at 0, then four `uint8_t` flags
 * at 4..7 and one pad byte — 8 bytes total, identical on both sides. Keep
 * field order, types and count in step with the Rust struct.
 */
typedef struct {
    int32_t status;         /**< HTTP status, or a negative transport status. */
    uint8_t json_valid;     /**< The body parsed as JSON. */
    uint8_t code_is_string; /**< `code` is present and is a string. */
    uint8_t expires_is_number; /**< `expires_in` is present and is a number. */
    uint8_t _pad;
} rf_pair_start_facts_t;

/**
 * Facts from a pair-claim response: HTTP status + what cJSON established.
 *
 * Same layout contract as `rf_pair_start_facts_t`: 8 bytes, `int32_t status`
 * at 0 then four `uint8_t` flags at 4..7. Asserted by the same Rust test.
 */
typedef struct {
    int32_t status;         /**< HTTP status, or a negative transport status. */
    uint8_t json_valid;     /**< The body parsed as JSON. */
    uint8_t token_is_string; /**< `token` is present and is a string. */
    /** `token` is a string with a first byte. Pass this separately:
     *  `cJSON_IsString` alone cannot tell an empty string from a non-empty
     *  one. */
    uint8_t token_nonempty;
    uint8_t _pad;
} rf_pair_claim_facts_t;

/**
 * Classify a pair-start response.
 * @return RF_PAIR_OUTCOME_PAIR_STARTED or RF_PAIR_OUTCOME_PAIR_START_FAILED
 *         (see `pairing.h`).
 */
uint8_t rf_pairing_classify_pair_start(const rf_pair_start_facts_t* f);

/**
 * Classify a pair-claim response.
 * @return RF_PAIR_OUTCOME_CLAIM_GRANTED / _PENDING / _REJECTED /
 *         _NETWORK_ERROR (see `pairing.h`).
 */
uint8_t rf_pairing_classify_claim(const rf_pair_claim_facts_t* f);

#ifdef __cplusplus
}
#endif

#endif  // PAIRING_RESPONSE_H
