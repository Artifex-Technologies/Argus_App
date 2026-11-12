#include "regioninfo.h"

#include <algorithm>
#include <cmath>

#include <QLoggingCategory>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonArray>
#include <QDebug>

Q_DECLARE_LOGGING_CATEGORY(CATEGORY_DEBUG)

// The bundle's values are checked as the Rust port checks them (flipper_updates::region, docs/RUST_PORT.md): a
// band's values are whole numbers that fit the firmware's fields (start and end 32 bits with start <= end,
// max_power -128..127 dBm, duty_cycle 0..100 %), every band name in a list is a string naming a band. The
// values used to be read with toInt (anything else became 0) and cut to 8 bits by RegionData, and an unknown
// name became an all-zero band. Both flags start false: they were left uninitialised when the text did not
// parse, so a broken reply could read as valid.
RegionInfo::RegionInfo():
    m_isValid(false),
    m_isError(false),
    m_errorCode(0)
{}

RegionInfo::RegionInfo(const QByteArray &text):
    RegionInfo()
{
    const auto doc = QJsonDocument::fromJson(text);

    if(doc.isNull()) {
        qCCritical(CATEGORY_DEBUG) << "Failed to parse the document";
        return;
    } else if(!doc.isObject()) {
        qCCritical(CATEGORY_DEBUG) << "Json document is not an object";
        return;
    }

    static const auto errorKey = QStringLiteral("error");
    static const auto successKey = QStringLiteral("success");

    const auto obj = doc.object();
    if(obj.contains(errorKey)) {
        parseError(obj.value(errorKey));
    } else if(obj.contains(successKey)) {
        parseSuccess(obj.value(successKey));
    } else {
        qCCritical(CATEGORY_DEBUG) << "Got valid JSON object, but neither success nor error info";
    }
}

bool RegionInfo::isValid() const
{
    return m_isValid;
}

bool RegionInfo::isError() const
{
    return m_isError;
}

bool RegionInfo::hasCountryCode() const
{
    return !m_country.isEmpty();
}

const QString &RegionInfo::errorString() const
{
    return m_errorString;
}

int RegionInfo::errorCode() const
{
    return m_errorCode;
}

const RegionInfo::CountryKey &RegionInfo::detectedCountry() const
{
    return m_country;
}

const RegionInfo::CountryKey RegionInfo::countryOr(const CountryKey &fallback) const
{
    return hasCountryCode() ? m_country : fallback;
}

const RegionInfo::BandKeyList &RegionInfo::defaultBandKeys() const
{
    return m_defaultBandKeys;
}

const RegionInfo::BandKeyList RegionInfo::countryBandKeys(const CountryKey &key) const
{
    return m_countries.contains(key) ? m_countries[key] : m_defaultBandKeys;
}

const RegionInfo::BandList RegionInfo::bandsByKeys(const BandKeyList &keys) const
{
    BandList ret;
    for(const auto &key : keys) {
        ret.append(m_bands.value(key));
    }
    return ret;
}

const RegionInfo::BandList RegionInfo::bandsByCountry(const CountryKey &key) const
{
    return bandsByKeys(countryBandKeys(key));
}

void RegionInfo::parseError(const QJsonValue &val)
{
    if(!val.isObject()) {
        qCCritical(CATEGORY_DEBUG) << "Error info is not an object";
        return;
    }

    const auto obj = val.toObject();

    static const auto codeKey = QStringLiteral("code");
    static const auto textKey = QStringLiteral("text");

    const auto isComplete = obj.contains(codeKey) && obj.contains(textKey);
    if(!isComplete) {
        qCCritical(CATEGORY_DEBUG) << "Error object is missing one or more fields";
        return;
    }

    m_errorCode = obj[codeKey].toInt();
    m_errorString = obj[textKey].toString();

    m_isValid = true;
    m_isError = true;
}

void RegionInfo::parseSuccess(const QJsonValue &val)
{
    if(!val.isObject()) {
        qCCritical(CATEGORY_DEBUG) << "Success info is not an object";
        return;
    }

    const auto obj = val.toObject();

    const auto bandsKey = QStringLiteral("bands");
    const auto countriesKey = QStringLiteral("countries");
    const auto countryKey = QStringLiteral("country");
    const auto defaultKey = QStringLiteral("default");

    const auto isComplete = obj.contains(bandsKey) && obj.contains(countriesKey) &&
                            obj.contains(countryKey) && obj.contains(defaultKey);
    if(!isComplete) {
        qCCritical(CATEGORY_DEBUG) << "Success object is missing one or more fields";
        return;
    }

    m_isError = false;
    m_isValid = parseBands(obj[bandsKey]) && parseCountries(obj[countriesKey]) &&
            parseCountry(obj[countryKey]) && parseDefault(obj[defaultKey]) && checkBandKeys();
}

bool RegionInfo::parseBandKeys(const QJsonValue &val, BandKeyList &keys, const char *what)
{
    if(!val.isArray()) {
        qCCritical(CATEGORY_DEBUG) << what << "is not an array";
        return false;
    }

    const QJsonArray arr = val.toArray();

    if(arr.isEmpty()) {
        qCCritical(CATEGORY_DEBUG) << what << "is an empty array";
        return false;
    }

    for(const auto &el : arr) {
        if(!el.isString()) {
            qCCritical(CATEGORY_DEBUG) << what << "holds a name that is not a string";
            return false;
        }

        keys.append(el.toString().toLocal8Bit());
    }

    return true;
}

bool RegionInfo::checkBandKeys() const
{
    auto allKnown = [this](const BandKeyList &keys) {
        return std::all_of(keys.cbegin(), keys.cend(), [this](const BandKey &key) { return m_bands.contains(key); });
    };

    const auto ok = allKnown(m_defaultBandKeys) &&
            std::all_of(m_countries.cbegin(), m_countries.cend(), allKnown);

    if(!ok) {
        qCCritical(CATEGORY_DEBUG) << "A bands list names a band that is not in bands";
    }

    return ok;
}

bool RegionInfo::parseBands(const QJsonValue &val)
{
    if(!val.isObject()) {
        qCCritical(CATEGORY_DEBUG) << "Bands info is not an object";
        return false;
    }

    const auto obj = val.toObject();

    if(obj.isEmpty()) {
        qCCritical(CATEGORY_DEBUG) << "Bands info is an empty object";
        return false;
    }

    const auto keys = obj.keys();
    for(const auto &key : keys) {
        if(!insertBand(key.toLocal8Bit(), obj[key])) {
            return false;
        }
    }

    return true;
}

bool RegionInfo::parseCountries(const QJsonValue &val)
{
    if(!val.isObject()) {
        qCCritical(CATEGORY_DEBUG) << "Countries info is not an object";
        return false;
    }

    const auto obj = val.toObject();

    if(obj.isEmpty()) {
        qCCritical(CATEGORY_DEBUG) << "Countries info is an empty object";
        return false;
    }

    const auto keys = obj.keys();
    for(const auto &key : keys) {
        if(!insertCountry(key.toLocal8Bit(), obj[key])) {
            return false;
        }
    }

    return true;
}

bool RegionInfo::parseCountry(const QJsonValue &val)
{
    if(!val.isString() && !val.isNull()) {
        qCCritical(CATEGORY_DEBUG) << "Country is not a string";
        return false;

    } else {
        m_country = val.toString().toLocal8Bit();
        return true;
    }
}

bool RegionInfo::parseDefault(const QJsonValue &val)
{
    return parseBandKeys(val, m_defaultBandKeys, "Default bands list");
}

bool RegionInfo::insertBand(const BandKey &key, const QJsonValue &val)
{
    if(!val.isObject()) {
        qCCritical(CATEGORY_DEBUG) << "Band info is not an object";
        return false;
    }

    const auto obj = val.toObject();

    const auto startKey = QStringLiteral("start");
    const auto endKey = QStringLiteral("end");
    const auto dutyCycleKey = QStringLiteral("duty_cycle");
    const auto maxPowerKey = QStringLiteral("max_power");

    const auto isComplete = obj.contains(startKey) && obj.contains(endKey) &&
                            obj.contains(dutyCycleKey) && obj.contains(maxPowerKey);

    if(!isComplete) {
        qCCritical(CATEGORY_DEBUG) << "Band object is missing one or more fields";
        return false;
    }

    // A whole number within [min, max].
    const auto integer = [](const QJsonValue &v, double min, double max, qint64 *out) {
        if(!v.isDouble()) {
            return false;
        }

        const auto d = v.toDouble();

        if(d != std::floor(d) || d < min || d > max) {
            return false;
        }

        *out = (qint64)d;
        return true;
    };

    qint64 start, end, power, duty;

    const auto inRange = integer(obj[startKey], 0, 4294967295.0, &start) && integer(obj[endKey], 0, 4294967295.0, &end) &&
            integer(obj[maxPowerKey], -128, 127, &power) && integer(obj[dutyCycleKey], 0, 100, &duty) && start <= end;

    if(!inRange) {
        qCCritical(CATEGORY_DEBUG) << "Band value out of range";
        return false;
    }

    const Band band = {
        (uint32_t)start,
        (uint32_t)end,
        (int32_t)power,
        (uint32_t)duty,
    };

    m_bands.insert(key, band);

    return true;
}

bool RegionInfo::insertCountry(const CountryKey &key, const QJsonValue &val)
{
    BandKeyList bandKeys;

    if(!parseBandKeys(val, bandKeys, "Country bands list")) {
        return false;
    }

    m_countries.insert(key, bandKeys);

    return true;
}
