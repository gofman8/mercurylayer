#include "../include/tx.h"

#include <openssl/sha.h>

#include <cstring>

namespace tx {
namespace {

/// A bounds-checked cursor. Every read goes through this, so a truncated or hostile buffer produces
/// a refusal rather than a read past the end — this parser runs in the process that holds every key
/// share, so "malformed input" must never be a memory-safety question.
class Cursor {
   public:
    explicit Cursor(const std::vector<unsigned char>& b) : buf_(b), pos_(0) {}

    bool take(size_t n, const unsigned char** out) {
        if (n > buf_.size() - pos_) return false;  // no overflow: pos_ <= size() always
        *out = buf_.data() + pos_;
        pos_ += n;
        return true;
    }
    bool u8(uint8_t* v) {
        const unsigned char* p;
        if (!take(1, &p)) return false;
        *v = *p;
        return true;
    }
    bool u32(uint32_t* v) {
        const unsigned char* p;
        if (!take(4, &p)) return false;
        *v = static_cast<uint32_t>(p[0]) | static_cast<uint32_t>(p[1]) << 8 |
             static_cast<uint32_t>(p[2]) << 16 | static_cast<uint32_t>(p[3]) << 24;
        return true;
    }
    bool u64(uint64_t* v) {
        const unsigned char* p;
        if (!take(8, &p)) return false;
        *v = 0;
        for (int i = 7; i >= 0; --i) *v = (*v << 8) | p[i];
        return true;
    }
    /// Bitcoin's CompactSize. Rejects non-canonical encodings: a value that fits in a shorter form
    /// but was written long is a different serialisation of the same transaction, and accepting both
    /// would let two byte strings claim the same txid.
    bool varint(uint64_t* v) {
        uint8_t tag;
        if (!u8(&tag)) return false;
        if (tag < 0xfd) {
            *v = tag;
            return true;
        }
        if (tag == 0xfd) {
            const unsigned char* p;
            if (!take(2, &p)) return false;
            *v = static_cast<uint64_t>(p[0]) | static_cast<uint64_t>(p[1]) << 8;
            return *v >= 0xfd;
        }
        if (tag == 0xfe) {
            uint32_t x;
            if (!u32(&x)) return false;
            *v = x;
            return *v > 0xffff;
        }
        if (!u64(v)) return false;
        return *v > 0xffffffffULL;
    }
    /// A length-prefixed byte string, refused if the length exceeds what is left. This is the check
    /// that stops a hostile `0xffffffffffffffff` length from becoming an allocation.
    bool var_bytes(std::vector<unsigned char>* out) {
        uint64_t n;
        if (!varint(&n)) return false;
        if (n > buf_.size() - pos_) return false;
        const unsigned char* p;
        if (!take(static_cast<size_t>(n), &p)) return false;
        out->assign(p, p + n);
        return true;
    }
    size_t remaining() const { return buf_.size() - pos_; }

   private:
    const std::vector<unsigned char>& buf_;
    size_t pos_;
};

std::vector<unsigned char> sha256(const unsigned char* d, size_t n) {
    std::vector<unsigned char> out(32);
    SHA256(d, n, out.data());
    return out;
}

void put_u32(std::vector<unsigned char>& v, uint32_t x) {
    v.push_back(static_cast<unsigned char>(x & 0xff));
    v.push_back(static_cast<unsigned char>((x >> 8) & 0xff));
    v.push_back(static_cast<unsigned char>((x >> 16) & 0xff));
    v.push_back(static_cast<unsigned char>((x >> 24) & 0xff));
}

void put_u64(std::vector<unsigned char>& v, uint64_t x) {
    for (int i = 0; i < 8; ++i) v.push_back(static_cast<unsigned char>((x >> (8 * i)) & 0xff));
}

void put_varint(std::vector<unsigned char>& v, uint64_t n) {
    if (n < 0xfd) {
        v.push_back(static_cast<unsigned char>(n));
    } else if (n <= 0xffff) {
        v.push_back(0xfd);
        v.push_back(static_cast<unsigned char>(n & 0xff));
        v.push_back(static_cast<unsigned char>((n >> 8) & 0xff));
    } else if (n <= 0xffffffffULL) {
        v.push_back(0xfe);
        put_u32(v, static_cast<uint32_t>(n));
    } else {
        v.push_back(0xff);
        put_u64(v, n);
    }
}

}  // namespace

std::vector<unsigned char> tagged_hash(const std::string& tag,
                                       const std::vector<unsigned char>& data) {
    const auto th = sha256(reinterpret_cast<const unsigned char*>(tag.data()), tag.size());
    std::vector<unsigned char> pre;
    pre.reserve(64 + data.size());
    pre.insert(pre.end(), th.begin(), th.end());
    pre.insert(pre.end(), th.begin(), th.end());
    pre.insert(pre.end(), data.begin(), data.end());
    return sha256(pre.data(), pre.size());
}

bool is_p2tr(const std::vector<unsigned char>& spk) {
    return spk.size() == 34 && spk[0] == 0x51 && spk[1] == 0x20;
}

std::vector<unsigned char> txid(const Transaction& t) {
    // The LEGACY serialisation: no segwit marker, no flag, no witness stack. Nothing here strips
    // witness data, because `parse` never kept any — so the legacy form is what this can produce,
    // and a future change that started keeping witnesses would have to change this function
    // deliberately rather than break it by omission.
    std::vector<unsigned char> ser;
    put_u32(ser, t.version);

    put_varint(ser, t.vin.size());
    for (const auto& in : t.vin) {
        ser.insert(ser.end(), in.prevout.txid, in.prevout.txid + 32);
        put_u32(ser, in.prevout.vout);
        put_varint(ser, in.script_sig.size());
        ser.insert(ser.end(), in.script_sig.begin(), in.script_sig.end());
        put_u32(ser, in.sequence);
    }

    put_varint(ser, t.vout.size());
    for (const auto& out : t.vout) {
        put_u64(ser, out.value);
        put_varint(ser, out.script_pubkey.size());
        ser.insert(ser.end(), out.script_pubkey.begin(), out.script_pubkey.end());
    }

    put_u32(ser, t.lock_time);

    const auto once = sha256(ser.data(), ser.size());
    return sha256(once.data(), once.size());
}

std::optional<std::vector<unsigned char>> p2tr_xonly_key(const std::vector<unsigned char>& spk) {
    if (!is_p2tr(spk)) return std::nullopt;
    return std::vector<unsigned char>(spk.begin() + 2, spk.end());
}

std::optional<Transaction> parse(const std::vector<unsigned char>& raw) {
    Cursor c(raw);
    Transaction t;
    if (!c.u32(&t.version)) return std::nullopt;

    uint64_t n_in;
    if (!c.varint(&n_in)) return std::nullopt;

    // Segwit marker: an input count of 0 followed by a non-zero flag. Read it, then re-read the real
    // input count. A zero flag is the forbidden encoding and is refused.
    bool segwit = false;
    if (n_in == 0) {
        uint8_t flag;
        if (!c.u8(&flag) || flag == 0) return std::nullopt;
        segwit = true;
        if (!c.varint(&n_in)) return std::nullopt;
    }
    if (n_in == 0) return std::nullopt;
    // An input is at least 41 bytes on the wire, so a count larger than the buffer can hold is a
    // hostile length rather than a big transaction. Refuse before reserving.
    if (n_in > c.remaining()) return std::nullopt;

    t.vin.reserve(static_cast<size_t>(n_in));
    for (uint64_t i = 0; i < n_in; ++i) {
        TxIn in;
        const unsigned char* p;
        if (!c.take(32, &p)) return std::nullopt;
        std::memcpy(in.prevout.txid, p, 32);
        if (!c.u32(&in.prevout.vout)) return std::nullopt;
        if (!c.var_bytes(&in.script_sig)) return std::nullopt;
        if (!c.u32(&in.sequence)) return std::nullopt;
        t.vin.push_back(std::move(in));
    }

    uint64_t n_out;
    if (!c.varint(&n_out)) return std::nullopt;
    if (n_out > c.remaining()) return std::nullopt;
    t.vout.reserve(static_cast<size_t>(n_out));
    for (uint64_t i = 0; i < n_out; ++i) {
        TxOut o;
        if (!c.u64(&o.value)) return std::nullopt;
        if (!c.var_bytes(&o.script_pubkey)) return std::nullopt;
        t.vout.push_back(std::move(o));
    }

    if (segwit) {
        // Witness stacks, parsed only to walk past them: the sighash commits to none of it.
        for (uint64_t i = 0; i < n_in; ++i) {
            uint64_t items;
            if (!c.varint(&items)) return std::nullopt;
            if (items > c.remaining()) return std::nullopt;
            for (uint64_t j = 0; j < items; ++j) {
                std::vector<unsigned char> item;
                if (!c.var_bytes(&item)) return std::nullopt;
            }
        }
    }

    if (!c.u32(&t.lock_time)) return std::nullopt;
    // Trailing bytes mean this is not exactly one transaction. Refuse rather than ignore them: a
    // parser that accepts a suffix accepts two different byte strings as the same transaction.
    if (c.remaining() != 0) return std::nullopt;
    return t;
}

std::optional<Transaction> parse_hex(const std::string& hex) {
    if (hex.size() % 2 != 0) return std::nullopt;
    std::vector<unsigned char> raw;
    raw.reserve(hex.size() / 2);
    auto nib = [](char ch) -> int {
        if (ch >= '0' && ch <= '9') return ch - '0';
        if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
        if (ch >= 'A' && ch <= 'F') return ch - 'A' + 10;
        return -1;
    };
    for (size_t i = 0; i < hex.size(); i += 2) {
        const int hi = nib(hex[i]), lo = nib(hex[i + 1]);
        if (hi < 0 || lo < 0) return std::nullopt;
        raw.push_back(static_cast<unsigned char>(hi << 4 | lo));
    }
    return parse(raw);
}

std::optional<std::vector<unsigned char>> taproot_key_path_sighash(
    const Transaction& t, const std::vector<Prevout>& prevouts, size_t input_index,
    unsigned char hash_type) {
    if (prevouts.size() != t.vin.size()) return std::nullopt;
    if (input_index >= t.vin.size()) return std::nullopt;
    // A scriptSig on a segwit spend means this is not the transaction shape we are hashing for.
    for (const auto& in : t.vin) {
        if (!in.script_sig.empty()) return std::nullopt;
    }

    std::vector<unsigned char> prevouts_ser, amounts_ser, spks_ser, seqs_ser, outputs_ser;
    for (size_t i = 0; i < t.vin.size(); ++i) {
        prevouts_ser.insert(prevouts_ser.end(), t.vin[i].prevout.txid, t.vin[i].prevout.txid + 32);
        put_u32(prevouts_ser, t.vin[i].prevout.vout);
        put_u64(amounts_ser, prevouts[i].value);
        put_varint(spks_ser, prevouts[i].script_pubkey.size());
        spks_ser.insert(spks_ser.end(), prevouts[i].script_pubkey.begin(),
                        prevouts[i].script_pubkey.end());
        put_u32(seqs_ser, t.vin[i].sequence);
    }
    for (const auto& o : t.vout) {
        put_u64(outputs_ser, o.value);
        put_varint(outputs_ser, o.script_pubkey.size());
        outputs_ser.insert(outputs_ser.end(), o.script_pubkey.begin(), o.script_pubkey.end());
    }

    const auto sha_prevouts = sha256(prevouts_ser.data(), prevouts_ser.size());
    const auto sha_amounts = sha256(amounts_ser.data(), amounts_ser.size());
    const auto sha_spks = sha256(spks_ser.data(), spks_ser.size());
    const auto sha_seqs = sha256(seqs_ser.data(), seqs_ser.size());
    const auto sha_outputs = sha256(outputs_ser.data(), outputs_ser.size());

    // BIP-341 sigMsg, key path, no annex. The hash type is SUPPLIED, not assumed: the tiers are
    // signed at SIGHASH_ALL (0x01), and hardcoding the 0x00 that BIP-341 calls "default" would make
    // every binding refuse every honest signature -- a failure that looks like a security feature.
    std::vector<unsigned char> msg;
    msg.push_back(0x00);  // sighash epoch
    msg.push_back(hash_type);
    put_u32(msg, t.version);
    put_u32(msg, t.lock_time);
    msg.insert(msg.end(), sha_prevouts.begin(), sha_prevouts.end());
    msg.insert(msg.end(), sha_amounts.begin(), sha_amounts.end());
    msg.insert(msg.end(), sha_spks.begin(), sha_spks.end());
    msg.insert(msg.end(), sha_seqs.begin(), sha_seqs.end());
    msg.insert(msg.end(), sha_outputs.begin(), sha_outputs.end());
    msg.push_back(0x00);  // spend_type: no annex, key path
    put_u32(msg, static_cast<uint32_t>(input_index));

    return tagged_hash("TapSighash", msg);
}

}  // namespace tx
