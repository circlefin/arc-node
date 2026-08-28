// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pragma solidity ^0.8.29;

import {Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

/**
 * @title PaymentEscrow
 * @notice Audited reference escrow contract for stablecoin payments on Arc Network.
 *
 * Security fixes applied:
 *   V1 — Arithmetic overflow guard on releaseDelay (max 90 days)
 *   V2 — Payee has unconditional release authority (no payer lockout)
 *   V3 — Fee rate snapshotted at fund time (not mutable at release)
 *   V4 — Ownable2Step prevents permanent ownership loss on typo
 */
contract PaymentEscrow is Ownable2Step {
    using SafeERC20 for IERC20;

    // ============ Constants ============

    /// @dev Maximum release delay (90 days) prevents arithmetic overflow + payer lockout.
    uint256 public constant MAX_RELEASE_DELAY = 90 days;

    /// @dev Basis points denominator (100% = 10_000).
    uint256 public constant BPS_DENOMINATOR = 10_000;

    // ============ Errors ============

    error EscrowNotFound();
    error EscrowAlreadyReleased();
    error DelayTooLarge();
    error AmountZero();
    error NotAuthorized();
    error TransferFailed();

    // ============ Storage ============

    struct Escrow {
        address payer;
        address payee;
        IERC20 token;
        uint256 amount;
        uint256 fundedAt;
        uint256 releaseDelay;
        uint16 feeBpsAtFund;  // V3: snapshotted at fund time
        bool released;
    }

    /// @dev Escrow ID → Escrow struct
    mapping(uint256 => Escrow) public escrows;

    /// @dev Protocol fee in basis points (e.g. 25 = 0.25%)
    uint256 public feeBps;

    /// @dev Protocol fee recipient
    address public feeRecipient;

    /// @dev Next escrow ID
    uint256 public nextEscrowId;

    // ============ Events ============

    event EscrowFunded(
        uint256 indexed id,
        address indexed payer,
        address indexed payee,
        IERC20 token,
        uint256 amount,
        uint256 releaseDelay,
        uint16 feeBpsAtFund
    );

    event EscrowReleased(
        uint256 indexed id,
        address indexed releasedBy,
        uint256 amountToPayee,
        uint256 fee
    );

    event FeeUpdated(uint256 oldFeeBps, uint256 newFeeBps);
    event FeeRecipientUpdated(address indexed oldRecipient, address indexed newRecipient);

    // ============ Constructor ============

    constructor(uint256 _feeBps, address _feeRecipient) {
        feeBps = _feeBps;
        feeRecipient = _feeRecipient;
    }

    // ============ Payer: fund escrow ============

    /**
     * @notice Fund a new escrow. Transfers `amount` of `token` from payer to this contract.
     * @param payee Address that will receive funds after release.
     * @param token ERC-20 token address (e.g. USDC on Arc).
     * @param amount Amount of tokens to escrow.
     * @param releaseDelay Seconds after `fundedAt` before automatic release is allowed.
     * @return id The escrow ID.
     *
     * Requirements:
     * - `payee` cannot be address(0).
     * - `amount` must be > 0.
     * - `releaseDelay` must be <= MAX_RELEASE_DELAY (V1 fix).
     * - Caller must have approved this contract to spend `amount` of `token`.
     */
    function fundEscrow(
        address payee,
        IERC20 token,
        uint256 amount,
        uint256 releaseDelay
    ) external returns (uint256 id) {
        if (payee == address(0)) revert NotAuthorized();
        if (amount == 0) revert AmountZero();
        if (releaseDelay > MAX_RELEASE_DELAY) revert DelayTooLarge(); // V1 fix

        id = nextEscrowId;
        unchecked { ++nextEscrowId; }

        escrows[id] = Escrow({
            payer: msg.sender,
            payee: payee,
            token: token,
            amount: amount,
            fundedAt: block.timestamp,
            releaseDelay: releaseDelay,
            feeBpsAtFund: uint16(feeBps),  // V3 fix: snapshot fee at fund time
            released: false
        });

        token.safeTransferFrom(msg.sender, address(this), amount);

        emit EscrowFunded(id, msg.sender, payee, token, amount, releaseDelay, uint16(feeBps));
    }

    // ============ Release escrow ============

    /**
     * @notice Release an escrow. Sends funds to payee minus fee.
     * @param id The escrow ID.
     *
     * Requirements:
     * - Escrow must exist and not already released.
     * - Caller must be payer, payee, or the release delay must have elapsed (V2 fix).
     */
    function releaseEscrow(uint256 id) external {
        Escrow storage e = escrows[id];

        if (e.payer == address(0)) revert EscrowNotFound();
        if (e.released) revert EscrowAlreadyReleased();

        // V2 fix: payee has unconditional release authority
        bool authorized = (
            msg.sender == e.payer
            || msg.sender == e.payee
            || block.timestamp >= e.fundedAt + e.releaseDelay
        );
        if (!authorized) revert NotAuthorized();

        e.released = true;

        uint256 fee = (e.amount * e.feeBpsAtFund) / BPS_DENOMINATOR;  // V3 fix: use snapshotted fee
        uint256 amountToPayee = e.amount - fee;

        if (fee > 0 && feeRecipient != address(0)) {
            e.token.safeTransfer(feeRecipient, fee);
        }
        e.token.safeTransfer(e.payee, amountToPayee);

        emit EscrowReleased(id, msg.sender, amountToPayee, fee);
    }

    // ============ View ============

    /**
     * @notice Get escrow details.
     */
    function getEscrow(uint256 id) external view returns (Escrow memory) {
        if (escrows[id].payer == address(0)) revert EscrowNotFound();
        return escrows[id];
    }

    // ============ Owner: protocol config ============

    /**
     * @notice Update the protocol fee. Only owner.
     * @param _feeBps New fee in basis points.
     */
    function setFee(uint256 _feeBps) external onlyOwner {
        emit FeeUpdated(feeBps, _feeBps);
        feeBps = _feeBps;
    }

    /**
     * @notice Update the fee recipient. Only owner.
     * @param _feeRecipient New fee recipient address.
     */
    function setFeeRecipient(address _feeRecipient) external onlyOwner {
        emit FeeRecipientUpdated(feeRecipient, _feeRecipient);
        feeRecipient = _feeRecipient;
    }
}
