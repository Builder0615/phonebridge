// macOS CoreBluetooth HOGP bridge for PhoneBridge.
//
// This file is intentionally a small platform bridge.  Rust owns the session,
// input mapping and lifecycle; this object owns only the CoreBluetooth GATT
// database and notification queue.

#import <CoreBluetooth/CoreBluetooth.h>
#import <Foundation/Foundation.h>

#include <dispatch/dispatch.h>
#include <stdint.h>
#include <string.h>

typedef void (*PBHIDStatusCallback)(void *context,
                                    int32_t powered_on,
                                    int32_t advertising,
                                    int32_t subscribed,
                                    int32_t connected,
                                    int32_t input_report_mask,
                                    int32_t error_code,
                                    int32_t manager_state,
                                    int32_t authorization,
                                    int32_t native_error_code);

// These values cross the small Rust/Objective-C ABI.  Keep them stable so the
// Rust side can turn an opaque CoreBluetooth state into an actionable log.
enum {
    PBHIDErrorNone = 0,
    PBHIDErrorStateUnknown = -2,
    PBHIDErrorStateResetting = -3,
    PBHIDErrorUnsupported = -4,
    PBHIDErrorUnauthorized = -5,
    PBHIDErrorPoweredOff = -6,
    PBHIDErrorService = -20,
    PBHIDErrorAdvertising = -21,
    // macOS 上外设角色的授权状态与处理器状态回调分离：授权「未确定」时
    // CoreBluetooth 仍会回调广播成功，但射频不会真正发包。用该码在 Rust
    // 侧产生权限引导，避免把假成功当作真广播。
    PBHIDErrorPermissionPending = -30,
};

// Input report subscription bits exposed through the small Rust/Objective-C
// status ABI.  A central may subscribe to battery or control characteristics
// during HID discovery before it subscribes to the actual input reports; those
// subscriptions must not be treated as usable keyboard/mouse input.
enum {
    PBHIDInputReportMouse = 1 << 0,
    PBHIDInputReportKeyboard = 1 << 1,
    PBHIDInputReportBootMouse = 1 << 2,
    PBHIDInputReportBootKeyboard = 1 << 3,
};

static void PBPerformOnMainQueueSync(dispatch_block_t block) {
    if ([NSThread isMainThread]) {
        block();
    } else {
        dispatch_sync(dispatch_get_main_queue(), block);
    }
}

static NSString *PBUUID(NSString *value) {
    // macOS CoreBluetooth rejects the short form for several system services,
    // including HID.  The canonical Bluetooth base UUID is still recognized by
    // iOS as the corresponding SIG service.
    return value;
}

static NSData *PBData(const uint8_t *bytes, NSUInteger length) {
    return [NSData dataWithBytes:bytes length:length];
}

// A full 128-bit HID service consumes 18 bytes in the legacy service-data
// field. CoreBluetooth reserves only 28 bytes for the foreground advertisement
// payload, so keep the local name at most 8 UTF-8 bytes (18 + 2 bytes of the
// local-name AD header + 8 = 28). An overlong name may push the HID UUID into
// the overflow area, which the iOS Settings app does not actively scan.
static NSString *PBCompactLocalName(NSString *name) {
    NSString *candidate = name.length > 0 ? name : @"KTP";
    NSData *utf8 = [candidate dataUsingEncoding:NSUTF8StringEncoding];
    if (utf8.length <= 8) {
        return candidate;
    }

    // Never cut through a UTF-8 code point. For 快投屏 (9 bytes), this
    // intentionally produces 快投 (6 bytes), leaving the HID UUID in the
    // primary advertisement packet.
    for (NSUInteger length = 8; length > 0; length--) {
        NSString *prefix = [[NSString alloc] initWithBytes:utf8.bytes
                                                     length:length
                                                   encoding:NSUTF8StringEncoding];
        if (prefix.length > 0) {
            return prefix;
        }
    }
    return @"KTP";
}

static NSString *PBCharacteristicKey(CBCharacteristic *characteristic) {
    return [NSString stringWithFormat:@"%p", characteristic];
}

// Standard HOGP report map.  The report IDs match the Report Reference
// descriptors installed below.  Keeping the optional system/consumer
// collections in the map makes the GATT layout match the HID hosts used by
// iOS; PhoneBridge only sends the mouse and keyboard IDs at present.
static const uint8_t kPhoneBridgeReportMap[] = {
    // Mouse, report ID 1.
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01,
    0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x03, 0x75, 0x01, 0x95, 0x03, 0x15, 0x00,
    0x25, 0x01, 0x81, 0x02, 0x95, 0x05, 0x81, 0x03,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38,
    0x75, 0x08, 0x95, 0x03, 0x15, 0x81, 0x25, 0x7F,
    0x81, 0x06, 0xC0, 0xC0,

    // Keyboard input and LED output, report IDs 2 and 3.
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x02,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x75, 0x01,
    0x95, 0x08, 0x15, 0x00, 0x25, 0x01, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x19, 0x00,
    0x29, 0xDD, 0x95, 0x06, 0x25, 0xDD, 0x81, 0x00,
    0x85, 0x03, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05,
    0x95, 0x05, 0x75, 0x01, 0x25, 0x01,
    0x91, 0x02, 0x95, 0x03, 0x91, 0x03, 0xC0,

    // Battery strength via the Battery Service, report ID 4.
    0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x04,
    0x05, 0x06, 0x09, 0x20, 0x75, 0x08, 0x95, 0x01,
    0x15, 0x00, 0x25, 0x64, 0x81, 0x02, 0xC0,

    // System control, report ID 5.
    0x05, 0x01, 0x09, 0x80, 0xA1, 0x01, 0x85, 0x05,
    0x09, 0x81, 0x09, 0x82, 0x09, 0x8E, 0x09, 0xA8,
    0x09, 0x8F, 0x09, 0x85, 0x09, 0x86, 0x09, 0xA7,
    0x75, 0x01, 0x95, 0x08, 0x15, 0x00, 0x25, 0x01,
    0x81, 0x06, 0xC0,

    // Consumer control, report ID 6.
    0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x06,
    0x19, 0x00, 0x2A, 0x74, 0x01, 0x75, 0x10, 0x95, 0x01,
    0x15, 0x00, 0x26, 0x74, 0x01, 0x81, 0x00, 0x1A,
    0x81, 0x01, 0x2A, 0xCB, 0x01, 0x95, 0x01, 0x75,
    0x08, 0x15, 0x01, 0x25, 0x4B, 0x81, 0x00, 0x1A,
    0x01, 0x02, 0x2A, 0xB0, 0x02, 0x25, 0xB0, 0x81,
    0x00, 0xA1, 0x03, 0x19, 0x00, 0x29, 0xFF, 0x95,
    0x01, 0x75, 0x08, 0x15, 0x00, 0x25, 0xFF, 0x81,
    0x00, 0xC0, 0xC0
};

// iOS's HOGP parser is sensitive to this exact report-map layout. Keep a
// compile-time guard so a padding/logical-range edit cannot silently break
// discovery or cause the central to unsubscribe during pairing.
_Static_assert(sizeof(kPhoneBridgeReportMap) == 239, "HOGP report map must be 239 bytes");

@interface PBHIDPeripheral : NSObject <CBPeripheralManagerDelegate> {
    dispatch_queue_t _queue;
    CBPeripheralManager *_manager;
    NSString *_localName;
    PBHIDStatusCallback _callback;
    void *_callbackContext;
    BOOL _wanted;
    BOOL _advertising;
    BOOL _servicesReady;

    CBUUID *_batteryServiceUUID;
    CBUUID *_deviceInfoServiceUUID;
    CBUUID *_hidServiceUUID;
    CBUUID *_batteryLevelUUID;
    CBUUID *_manufacturerNameUUID;
    CBUUID *_modelNumberUUID;
    CBUUID *_pnpIDUUID;
    CBUUID *_hidInformationUUID;
    CBUUID *_reportMapUUID;
    CBUUID *_hidControlPointUUID;
    CBUUID *_protocolModeUUID;
    CBUUID *_reportUUID;
    CBUUID *_bootMouseInputUUID;
    CBUUID *_bootKeyboardInputUUID;
    CBUUID *_bootKeyboardOutputUUID;
    CBUUID *_reportReferenceUUID;
    CBUUID *_externalReportReferenceUUID;

    CBMutableService *_batteryService;
    CBMutableService *_deviceInfoService;
    CBMutableService *_hidService;
    CBMutableCharacteristic *_batteryLevel;
    CBMutableCharacteristic *_mouseReport;
    CBMutableCharacteristic *_keyboardReport;
    CBMutableCharacteristic *_keyboardLEDReport;
    CBMutableCharacteristic *_systemReport;
    CBMutableCharacteristic *_consumerReport;
    CBMutableCharacteristic *_bootMouseInput;
    CBMutableCharacteristic *_bootKeyboardInput;

    NSMutableDictionary<NSNumber *, CBMutableCharacteristic *> *_reportsByID;
    NSMutableDictionary<NSString *, CBMutableCharacteristic *> *_characteristicsByKey;
    NSMutableDictionary<NSNumber *, NSData *> *_cachedReports;
    NSMutableDictionary<NSString *, NSMutableArray<NSData *> *> *_pendingReports;
    NSMutableDictionary<NSUUID *, CBCentral *> *_centrals;
    NSMutableDictionary<NSUUID *, NSMutableSet<NSString *> *> *_subscriptions;
}

- (instancetype)initWithCallback:(PBHIDStatusCallback)callback
                           context:(void *)context
                         localName:(NSString *)localName;
- (void)performSync:(dispatch_block_t)block;
- (void)ensureManager;
- (void)emitStatus:(int32_t)errorCode nativeErrorCode:(int32_t)nativeErrorCode;
- (int)start;
- (void)stop;
- (int)sendKeyboard:(NSData *)data;
- (int)sendMouse:(NSData *)data;
@end

@implementation PBHIDPeripheral

- (instancetype)initWithCallback:(PBHIDStatusCallback)callback
                           context:(void *)context
                         localName:(NSString *)localName {
    self = [super init];
    if (!self) {
        return nil;
    }

    _callback = callback;
    _callbackContext = context;
    // Keep the full product name in the app UI, but use a packet-safe local
    // name for CoreBluetooth advertising. macOS may still show its computer
    // / GAP name in iOS Settings; Rust exposes that pairing name separately.
    _localName = [PBCompactLocalName(localName ?: @"快投屏") copy];
    // CoreBluetooth is owned by the app's main run loop.  Tauri invokes Rust
    // commands on worker threads, so using the main queue here avoids creating
    // the peripheral manager on a transient command queue and makes the
    // manager's state/permission callbacks deterministic on macOS.
    _queue = dispatch_get_main_queue();
    _reportsByID = [NSMutableDictionary dictionary];
    _characteristicsByKey = [NSMutableDictionary dictionary];
    _cachedReports = [NSMutableDictionary dictionary];
    _pendingReports = [NSMutableDictionary dictionary];
    _centrals = [NSMutableDictionary dictionary];
    _subscriptions = [NSMutableDictionary dictionary];

    _batteryServiceUUID = [CBUUID UUIDWithString:PBUUID(@"0000180F-0000-1000-8000-00805F9B34FB")];
    _deviceInfoServiceUUID = [CBUUID UUIDWithString:PBUUID(@"0000180A-0000-1000-8000-00805F9B34FB")];
    _hidServiceUUID = [CBUUID UUIDWithString:PBUUID(@"00001812-0000-1000-8000-00805F9B34FB")];
    _batteryLevelUUID = [CBUUID UUIDWithString:PBUUID(@"00002A19-0000-1000-8000-00805F9B34FB")];
    _manufacturerNameUUID = [CBUUID UUIDWithString:PBUUID(@"00002A29-0000-1000-8000-00805F9B34FB")];
    _modelNumberUUID = [CBUUID UUIDWithString:PBUUID(@"00002A24-0000-1000-8000-00805F9B34FB")];
    _pnpIDUUID = [CBUUID UUIDWithString:PBUUID(@"00002A50-0000-1000-8000-00805F9B34FB")];
    _hidInformationUUID = [CBUUID UUIDWithString:PBUUID(@"00002A4A-0000-1000-8000-00805F9B34FB")];
    _reportMapUUID = [CBUUID UUIDWithString:PBUUID(@"00002A4B-0000-1000-8000-00805F9B34FB")];
    _hidControlPointUUID = [CBUUID UUIDWithString:PBUUID(@"00002A4C-0000-1000-8000-00805F9B34FB")];
    _protocolModeUUID = [CBUUID UUIDWithString:PBUUID(@"00002A4E-0000-1000-8000-00805F9B34FB")];
    _reportUUID = [CBUUID UUIDWithString:PBUUID(@"00002A4D-0000-1000-8000-00805F9B34FB")];
    _bootMouseInputUUID = [CBUUID UUIDWithString:PBUUID(@"00002A33-0000-1000-8000-00805F9B34FB")];
    _bootKeyboardInputUUID = [CBUUID UUIDWithString:PBUUID(@"00002A22-0000-1000-8000-00805F9B34FB")];
    _bootKeyboardOutputUUID = [CBUUID UUIDWithString:PBUUID(@"00002A32-0000-1000-8000-00805F9B34FB")];
    _reportReferenceUUID = [CBUUID UUIDWithString:PBUUID(@"00002908-0000-1000-8000-00805F9B34FB")];
    _externalReportReferenceUUID = [CBUUID UUIDWithString:PBUUID(@"00002907-0000-1000-8000-00805F9B34FB")];

    uint8_t zeroMouse[] = {0, 0, 0, 0};
    uint8_t zeroKeyboard[] = {0, 0, 0, 0, 0, 0, 0, 0};
    uint8_t zeroLed[] = {0};
    uint8_t battery[] = {100};
    uint8_t zeroSystem[] = {0};
    uint8_t zeroConsumer[] = {0, 0, 0, 0, 0};
    _cachedReports[@1] = PBData(zeroMouse, sizeof(zeroMouse));
    _cachedReports[@2] = PBData(zeroKeyboard, sizeof(zeroKeyboard));
    _cachedReports[@3] = PBData(zeroLed, sizeof(zeroLed));
    _cachedReports[@4] = PBData(battery, sizeof(battery));
    _cachedReports[@5] = PBData(zeroSystem, sizeof(zeroSystem));
    _cachedReports[@6] = PBData(zeroConsumer, sizeof(zeroConsumer));

    // Create CBPeripheralManager only when the user explicitly enables iOS
    // control.  This matches Apple's peripheral-role lifecycle and avoids
    // losing the initial state callback before Rust has installed its status
    // callback.
    return self;
}

- (void)performSync:(dispatch_block_t)block {
    PBPerformOnMainQueueSync(block);
}

- (void)ensureManager {
    if (_manager) {
        return;
    }
    _manager = [[CBPeripheralManager alloc] initWithDelegate:self
                                                       queue:_queue
                                                     options:@{CBPeripheralManagerOptionShowPowerAlertKey : @YES}];
}

- (BOOL)isPoweredOn {
    return _manager && _manager.state == CBManagerStatePoweredOn;
}

- (BOOL)hasSubscribersForCharacteristic:(CBCharacteristic *)characteristic {
    if (!characteristic) {
        return NO;
    }
    NSString *key = PBCharacteristicKey(characteristic);
    for (NSSet<NSString *> *values in _subscriptions.allValues) {
        if ([values containsObject:key]) {
            return YES;
        }
    }
    return NO;
}

- (int32_t)inputReportMask {
    int32_t mask = 0;
    if ([self hasSubscribersForCharacteristic:_mouseReport]) {
        mask |= PBHIDInputReportMouse;
    }
    if ([self hasSubscribersForCharacteristic:_keyboardReport]) {
        mask |= PBHIDInputReportKeyboard;
    }
    if ([self hasSubscribersForCharacteristic:_bootMouseInput]) {
        mask |= PBHIDInputReportBootMouse;
    }
    if ([self hasSubscribersForCharacteristic:_bootKeyboardInput]) {
        mask |= PBHIDInputReportBootKeyboard;
    }
    return mask;
}

- (int32_t)stateErrorCode {
    // The authorization class property is available on macOS 10.15+, which
    // is below this app's macOS 13 minimum.  Check it first because a denied
    // permission can otherwise leave the manager in the initial Unknown state.
    CBManagerAuthorization authorization = [CBManager authorization];
    if (authorization == CBManagerAuthorizationRestricted ||
        authorization == CBManagerAuthorizationDenied) {
        return PBHIDErrorUnauthorized;
    }
    switch (_manager.state) {
        case CBManagerStateUnknown:
            return PBHIDErrorStateUnknown;
        case CBManagerStateResetting:
            return PBHIDErrorStateResetting;
        case CBManagerStateUnsupported:
            return PBHIDErrorUnsupported;
        case CBManagerStateUnauthorized:
            return PBHIDErrorUnauthorized;
        case CBManagerStatePoweredOff:
            return PBHIDErrorPoweredOff;
        case CBManagerStatePoweredOn:
            return PBHIDErrorNone;
    }
    return PBHIDErrorStateUnknown;
}

- (void)emitStatus:(int32_t)errorCode {
    [self emitStatus:errorCode nativeErrorCode:0];
}

- (void)emitStatus:(int32_t)errorCode nativeErrorCode:(int32_t)nativeErrorCode {
    if (!_callback) {
        return;
    }
    int32_t inputReportMask = [self inputReportMask];
    int32_t effectiveError = errorCode != PBHIDErrorNone ? errorCode : [self stateErrorCode];
    _callback(_callbackContext,
              [self isPoweredOn] ? 1 : 0,
              _advertising ? 1 : 0,
              inputReportMask != 0 ? 1 : 0,
              inputReportMask != 0 ? 1 : 0,
              inputReportMask,
              effectiveError,
              (int32_t)_manager.state,
              (int32_t)[CBManager authorization],
              nativeErrorCode);
}

- (CBMutableDescriptor *)reportReferenceForID:(uint8_t)reportID type:(uint8_t)type {
    uint8_t bytes[] = {reportID, type};
    return [[CBMutableDescriptor alloc] initWithType:_reportReferenceUUID
                                               value:PBData(bytes, sizeof(bytes))];
}

- (void)rememberCharacteristic:(CBMutableCharacteristic *)characteristic {
    _characteristicsByKey[PBCharacteristicKey(characteristic)] = characteristic;
}

- (CBMutableCharacteristic *)reportCharacteristicForID:(uint8_t)reportID {
    return _reportsByID[@(reportID)];
}

- (CBMutableCharacteristic *)makeReportCharacteristic:(uint8_t)reportID {
    CBMutableCharacteristic *characteristic = [[CBMutableCharacteristic alloc]
        initWithType:_reportUUID
           properties:(CBCharacteristicPropertyRead | CBCharacteristicPropertyNotifyEncryptionRequired)
                value:nil
          permissions:CBAttributePermissionsReadEncryptionRequired];
    characteristic.descriptors = @[[self reportReferenceForID:reportID type:1]];
    _reportsByID[@(reportID)] = characteristic;
    [self rememberCharacteristic:characteristic];
    return characteristic;
}

- (CBMutableService *)buildBatteryService {
    CBMutableService *service = [[CBMutableService alloc] initWithType:_batteryServiceUUID primary:YES];
    _batteryLevel = [[CBMutableCharacteristic alloc]
        initWithType:_batteryLevelUUID
           properties:(CBCharacteristicPropertyRead | CBCharacteristicPropertyNotifyEncryptionRequired)
                value:nil
          permissions:CBAttributePermissionsReadEncryptionRequired];
    _batteryLevel.descriptors = @[[[CBMutableDescriptor alloc]
        initWithType:_reportReferenceUUID
               value:PBData((const uint8_t[]){4, 1}, 2)]];
    service.characteristics = @[_batteryLevel];
    [self rememberCharacteristic:_batteryLevel];
    return service;
}

- (CBMutableService *)buildDeviceInfoService {
    CBMutableService *service = [[CBMutableService alloc] initWithType:_deviceInfoServiceUUID primary:YES];
    NSData *manufacturer = [@"PhoneBridge" dataUsingEncoding:NSUTF8StringEncoding];
    NSData *model = [@"PhoneBridge macOS" dataUsingEncoding:NSUTF8StringEncoding];
    NSData *pnp = PBData((const uint8_t[]){1, 0xFF, 0xFF, 1, 0, 0, 1}, 7);
    CBMutableCharacteristic *manufacturerCharacteristic = [[CBMutableCharacteristic alloc]
        initWithType:_manufacturerNameUUID
           properties:CBCharacteristicPropertyRead
                value:manufacturer
          permissions:CBAttributePermissionsReadable];
    CBMutableCharacteristic *modelCharacteristic = [[CBMutableCharacteristic alloc]
        initWithType:_modelNumberUUID
           properties:CBCharacteristicPropertyRead
                value:model
          permissions:CBAttributePermissionsReadable];
    CBMutableCharacteristic *pnpCharacteristic = [[CBMutableCharacteristic alloc]
        initWithType:_pnpIDUUID
           properties:CBCharacteristicPropertyRead
                value:pnp
          permissions:CBAttributePermissionsReadable];
    service.characteristics = @[manufacturerCharacteristic, modelCharacteristic, pnpCharacteristic];
    [self rememberCharacteristic:manufacturerCharacteristic];
    [self rememberCharacteristic:modelCharacteristic];
    [self rememberCharacteristic:pnpCharacteristic];
    return service;
}

- (CBMutableService *)buildHIDService {
    CBMutableService *service = [[CBMutableService alloc] initWithType:_hidServiceUUID primary:YES];
    if (_batteryService) {
        service.includedServices = @[_batteryService];
    }

    CBMutableCharacteristic *controlPoint = [[CBMutableCharacteristic alloc]
        initWithType:_hidControlPointUUID
           properties:CBCharacteristicPropertyRead
                value:nil
          permissions:CBAttributePermissionsReadEncryptionRequired];
    CBMutableCharacteristic *protocolMode = [[CBMutableCharacteristic alloc]
        initWithType:_protocolModeUUID
           properties:(CBCharacteristicPropertyRead | CBCharacteristicPropertyWriteWithoutResponse)
                value:nil
          permissions:(CBAttributePermissionsReadEncryptionRequired | CBAttributePermissionsWriteEncryptionRequired)];
    uint8_t hidInfoBytes[] = {0x11, 0x01, 0x00, 0x03};
    CBMutableCharacteristic *hidInfo = [[CBMutableCharacteristic alloc]
        initWithType:_hidInformationUUID
           properties:CBCharacteristicPropertyRead
                value:PBData(hidInfoBytes, sizeof(hidInfoBytes))
          permissions:CBAttributePermissionsReadEncryptionRequired];

    _bootMouseInput = [[CBMutableCharacteristic alloc]
        initWithType:_bootMouseInputUUID
           properties:(CBCharacteristicPropertyRead | CBCharacteristicPropertyNotifyEncryptionRequired)
                value:nil
          permissions:(CBAttributePermissionsReadEncryptionRequired | CBAttributePermissionsWriteEncryptionRequired)];
    _bootKeyboardInput = [[CBMutableCharacteristic alloc]
        initWithType:_bootKeyboardInputUUID
           properties:(CBCharacteristicPropertyRead | CBCharacteristicPropertyNotifyEncryptionRequired)
                value:nil
          permissions:(CBAttributePermissionsReadEncryptionRequired | CBAttributePermissionsWriteEncryptionRequired)];
    CBMutableCharacteristic *bootKeyboardOutput = [[CBMutableCharacteristic alloc]
        initWithType:_bootKeyboardOutputUUID
           properties:(CBCharacteristicPropertyRead | CBCharacteristicPropertyWriteWithoutResponse | CBCharacteristicPropertyWrite)
                value:nil
          permissions:(CBAttributePermissionsReadEncryptionRequired | CBAttributePermissionsWriteEncryptionRequired)];

    CBMutableCharacteristic *reportMap = [[CBMutableCharacteristic alloc]
        initWithType:_reportMapUUID
           properties:CBCharacteristicPropertyRead
                value:PBData(kPhoneBridgeReportMap, sizeof(kPhoneBridgeReportMap))
          permissions:CBAttributePermissionsReadEncryptionRequired];
    reportMap.descriptors = @[[[CBMutableDescriptor alloc]
        initWithType:_externalReportReferenceUUID
               value:PBData((const uint8_t[]){0x19, 0x2A}, 2)]];

    _mouseReport = [self makeReportCharacteristic:1];
    _keyboardReport = [self makeReportCharacteristic:2];
    _systemReport = [self makeReportCharacteristic:5];
    _consumerReport = [self makeReportCharacteristic:6];
    _keyboardLEDReport = [[CBMutableCharacteristic alloc]
        initWithType:_reportUUID
           properties:(CBCharacteristicPropertyRead | CBCharacteristicPropertyWriteWithoutResponse | CBCharacteristicPropertyWrite)
                value:nil
          permissions:(CBAttributePermissionsReadEncryptionRequired | CBAttributePermissionsWriteEncryptionRequired)];
    _keyboardLEDReport.descriptors = @[[self reportReferenceForID:3 type:2]];
    [self rememberCharacteristic:_keyboardLEDReport];

    service.characteristics = @[controlPoint,
                                protocolMode,
                                hidInfo,
                                _bootMouseInput,
                                _bootKeyboardInput,
                                bootKeyboardOutput,
                                reportMap,
                                _systemReport,
                                _consumerReport,
                                _mouseReport,
                                _keyboardReport,
                                _keyboardLEDReport];
    [self rememberCharacteristic:controlPoint];
    [self rememberCharacteristic:protocolMode];
    [self rememberCharacteristic:hidInfo];
    [self rememberCharacteristic:_bootMouseInput];
    [self rememberCharacteristic:_bootKeyboardInput];
    [self rememberCharacteristic:bootKeyboardOutput];
    [self rememberCharacteristic:reportMap];
    return service;
}

- (void)installServicesIfNeeded {
    if (!_wanted || ![self isPoweredOn] || _batteryService || _servicesReady) {
        return;
    }
    // GAP（0x1800）和 GATT（0x1801）是 CoreBluetooth 由系统维护的保留服务，
    // 应用不能像普通服务一样 addService；此前从 GAP 开始发布会在这里失败，
    // 后续 HID 服务永远不会添加。设备名通过广播数据提供；macOS 也可能
    // 在 iOS 系统蓝牙列表中使用本机的 GAP/电脑名称，而不是应用名称。
    _batteryService = [self buildBatteryService];
    [_manager addService:_batteryService];
}

- (void)startAdvertisingNow {
    if (!_wanted || !_hidService || _advertising) {
        return;
    }
    [_manager startAdvertising:@{
        CBAdvertisementDataLocalNameKey : _localName ?: @"KTP",
        CBAdvertisementDataServiceUUIDsKey : @[_hidServiceUUID]
    }];
}

- (int)start {
    __block int32_t result = 0;
    [self performSync:^{
        @autoreleasepool {
            _wanted = YES;
            [self ensureManager];
            int32_t stateError = [self stateErrorCode];
            if (stateError == PBHIDErrorUnsupported ||
                stateError == PBHIDErrorUnauthorized ||
                stateError == PBHIDErrorPoweredOff) {
                [self emitStatus:stateError];
                result = stateError;
                return;
            }
            if ([self isPoweredOn]) {
                [self installServicesIfNeeded];
            }
            // CoreBluetooth reports PoweredOn asynchronously; Unknown and
            // Resetting are pending states.  The delegate will finish service
            // publication after the manager becomes available.
            [self emitStatus:0];
        }
    }];
    return result;
}

- (void)stop {
    [self performSync:^{
        @autoreleasepool {
            _wanted = NO;
            [_manager stopAdvertising];
            [_manager removeAllServices];
            _advertising = NO;
            _servicesReady = NO;
            _batteryService = nil;
            _deviceInfoService = nil;
            _hidService = nil;
            _batteryLevel = nil;
            _mouseReport = nil;
            _keyboardReport = nil;
            _keyboardLEDReport = nil;
            _systemReport = nil;
            _consumerReport = nil;
            _bootMouseInput = nil;
            _bootKeyboardInput = nil;
            [_reportsByID removeAllObjects];
            [_characteristicsByKey removeAllObjects];
            [_pendingReports removeAllObjects];
            [_centrals removeAllObjects];
            [_subscriptions removeAllObjects];
            [self emitStatus:0];
        }
    }];
}

- (NSArray<CBCentral *> *)recipientsForCharacteristic:(CBCharacteristic *)characteristic {
    NSString *characteristicKey = PBCharacteristicKey(characteristic);
    NSMutableArray<CBCentral *> *recipients = [NSMutableArray array];
    [_subscriptions enumerateKeysAndObjectsUsingBlock:^(NSUUID *key, NSSet<NSString *> *set, BOOL *stop) {
        (void)stop;
        if ([set containsObject:characteristicKey]) {
            CBCentral *central = self->_centrals[key];
            if (central) {
                [recipients addObject:central];
            }
        }
    }];
    return recipients;
}

- (BOOL)updateData:(NSData *)data forCharacteristic:(CBMutableCharacteristic *)characteristic {
    NSArray<CBCentral *> *recipients = [self recipientsForCharacteristic:characteristic];
    if (recipients.count == 0) {
        return NO;
    }
    BOOL accepted = [_manager updateValue:data forCharacteristic:characteristic onSubscribedCentrals:recipients];
    if (!accepted) {
        NSString *key = PBCharacteristicKey(characteristic);
        NSMutableArray<NSData *> *queue = _pendingReports[key];
        if (!queue) {
            queue = [NSMutableArray array];
            _pendingReports[key] = queue;
        }
        // Keep a bounded FIFO. Keyboard reports must retain their order;
        // dropping the oldest item is preferable to allowing a stalled BLE
        // central to grow an unbounded queue. ReleaseAll is sent on every
        // focus/disconnect path, so the next input sequence can recover.
        if (queue.count >= 64) {
            [queue removeObjectAtIndex:0];
        }
        [queue addObject:data];
    }
    // YES means that at least one central subscribed to this characteristic;
    // the value may have been queued when CoreBluetooth back-pressured us.
    return YES;
}

- (void)drainPendingReports {
    for (NSString *key in [_pendingReports.allKeys copy]) {
        CBMutableCharacteristic *characteristic = _characteristicsByKey[key];
        NSMutableArray<NSData *> *queue = _pendingReports[key];
        if (!characteristic || queue.count == 0) {
            [_pendingReports removeObjectForKey:key];
            continue;
        }
        NSArray<CBCentral *> *recipients = [self recipientsForCharacteristic:characteristic];
        if (recipients.count == 0) {
            [_pendingReports removeObjectForKey:key];
            continue;
        }
        while (queue.count > 0) {
            NSData *data = queue.firstObject;
            if (![_manager updateValue:data
                   forCharacteristic:characteristic
                onSubscribedCentrals:recipients]) {
                break;
            }
            [queue removeObjectAtIndex:0];
        }
        if (queue.count == 0) {
            [_pendingReports removeObjectForKey:key];
        }
    }
}

- (NSData *)bootMouseData {
    NSData *data = _cachedReports[@1] ?: [NSData data];
    // Boot mouse reports are buttons + X + Y (3 bytes).  The normal Report
    // characteristic additionally carries the wheel byte.
    return data.length > 3 ? [data subdataWithRange:NSMakeRange(0, 3)] : data;
}

- (int)sendData:(NSData *)data reportID:(uint8_t)reportID {
    if (!_wanted || ![self isPoweredOn] || [self inputReportMask] == 0) {
        return -1;
    }
    CBMutableCharacteristic *report = [self reportCharacteristicForID:reportID];
    if (!report) {
        return -1;
    }
    _cachedReports[@(reportID)] = data;
    BOOL delivered = NO;
    if ([self hasSubscribersForCharacteristic:report]) {
        delivered = [self updateData:data forCharacteristic:report] || delivered;
    }
    if (reportID == 1 && _bootMouseInput) {
        if ([self hasSubscribersForCharacteristic:_bootMouseInput]) {
            delivered = [self updateData:[self bootMouseData]
                        forCharacteristic:_bootMouseInput] || delivered;
        }
    } else if (reportID == 2 && _bootKeyboardInput) {
        if ([self hasSubscribersForCharacteristic:_bootKeyboardInput]) {
            delivered = [self updateData:data
                        forCharacteristic:_bootKeyboardInput] || delivered;
        }
    }
    // A battery/control-point subscription alone is not enough to make input
    // usable. Report this separately so Rust can surface a useful diagnostic
    // instead of silently claiming that the key/mouse event was sent.
    return delivered ? 0 : -2;
}

- (int)sendKeyboard:(NSData *)data {
    __block int result = -1;
    [self performSync:^{
        @autoreleasepool {
            result = [self sendData:data reportID:2];
        }
    }];
    return result;
}

- (int)sendMouse:(NSData *)data {
    __block int result = -1;
    [self performSync:^{
        @autoreleasepool {
            result = [self sendData:data reportID:1];
        }
    }];
    return result;
}

- (NSData *)valueForCharacteristic:(CBCharacteristic *)characteristic {
    if ([characteristic.UUID isEqual:_batteryLevelUUID]) {
        return _cachedReports[@4];
    }
    if ([characteristic.UUID isEqual:_hidInformationUUID]) {
        return PBData((const uint8_t[]){0x11, 0x01, 0x00, 0x03}, 4);
    }
    if ([characteristic.UUID isEqual:_reportMapUUID]) {
        return PBData(kPhoneBridgeReportMap, sizeof(kPhoneBridgeReportMap));
    }
    if ([characteristic.UUID isEqual:_protocolModeUUID]) {
        return PBData((const uint8_t[]){1}, 1);
    }
    if ([characteristic.UUID isEqual:_manufacturerNameUUID]) {
        return [@"PhoneBridge" dataUsingEncoding:NSUTF8StringEncoding];
    }
    if ([characteristic.UUID isEqual:_modelNumberUUID]) {
        return [@"PhoneBridge macOS" dataUsingEncoding:NSUTF8StringEncoding];
    }
    if ([characteristic.UUID isEqual:_pnpIDUUID]) {
        return PBData((const uint8_t[]){1, 0xFF, 0xFF, 1, 0, 0, 1}, 7);
    }
    if ([characteristic.UUID isEqual:_bootMouseInputUUID]) {
        return [self bootMouseData];
    }
    if ([characteristic.UUID isEqual:_bootKeyboardInputUUID]) {
        return _cachedReports[@2];
    }
    for (NSNumber *number in _reportsByID) {
        // Report characteristics share UUID 0x2A4D; compare object identity,
        // not UUID-based `isEqual:`, so report ID 1/2 cannot be mixed up.
        if (_reportsByID[number] == characteristic) {
            return _cachedReports[number] ?: [NSData data];
        }
    }
    return [NSData data];
}

- (void)handleOutputWrite:(CBATTRequest *)request {
    // LED/output reports are accepted so iOS completes the HID handshake.  The
    // current UI does not need to expose Caps Lock state back to Rust.
    (void)request;
}

- (void)peripheralManagerDidUpdateState:(CBPeripheralManager *)peripheral {
    dispatch_assert_queue(_queue);
    if (peripheral.state == CBManagerStatePoweredOn) {
        [self installServicesIfNeeded];
    } else {
        _advertising = NO;
        _servicesReady = NO;
        [_subscriptions removeAllObjects];
        [_centrals removeAllObjects];
        _batteryService = nil;
        _deviceInfoService = nil;
        _hidService = nil;
        _batteryLevel = nil;
        _mouseReport = nil;
        _keyboardReport = nil;
        _keyboardLEDReport = nil;
        _systemReport = nil;
        _consumerReport = nil;
        _bootMouseInput = nil;
        _bootKeyboardInput = nil;
        [_reportsByID removeAllObjects];
        [_characteristicsByKey removeAllObjects];
        [_pendingReports removeAllObjects];
    }
    [self emitStatus:0];
}

- (void)peripheralManager:(CBPeripheralManager *)peripheral
             didAddService:(CBService *)service
                    error:(NSError *)error {
    dispatch_assert_queue(_queue);
    if (!_wanted) {
        return;
    }
    if (error) {
        _servicesReady = NO;
        // 清空本轮异步发布结果，允许用户停止后重新启用控制时重新建表。
        [_manager removeAllServices];
        _batteryService = nil;
        _deviceInfoService = nil;
        _hidService = nil;
        _batteryLevel = nil;
        _mouseReport = nil;
        _keyboardReport = nil;
        _keyboardLEDReport = nil;
        _systemReport = nil;
        _consumerReport = nil;
        _bootMouseInput = nil;
        _bootKeyboardInput = nil;
        [_reportsByID removeAllObjects];
        [_characteristicsByKey removeAllObjects];
        [self emitStatus:PBHIDErrorService nativeErrorCode:(int32_t)error.code];
        return;
    }
    if ([service.UUID isEqual:_batteryServiceUUID]) {
        _deviceInfoService = [self buildDeviceInfoService];
        [peripheral addService:_deviceInfoService];
    } else if ([service.UUID isEqual:_deviceInfoServiceUUID]) {
        _hidService = [self buildHIDService];
        [peripheral addService:_hidService];
    } else if ([service.UUID isEqual:_hidServiceUUID]) {
        _servicesReady = YES;
        [self startAdvertisingNow];
    }
}

- (void)peripheralManagerDidStartAdvertising:(CBPeripheralManager *)peripheral
                                        error:(NSError *)error {
    dispatch_assert_queue(_queue);
    // The callback is the authoritative completion signal. Keep the native
    // flag aligned with CoreBluetooth's observable state as well, so a
    // callback with a nil error cannot be reported as a usable broadcast if
    // the manager immediately stopped advertising.
    _advertising = error == nil && peripheral.isAdvertising;
    if (error) {
        [self emitStatus:PBHIDErrorAdvertising nativeErrorCode:(int32_t)error.code];
        return;
    }
    if (!_advertising) {
        [self emitStatus:PBHIDErrorAdvertising nativeErrorCode:0];
        return;
    }
    // macOS 外设角色权限与状态回调分离：授权未被用户确认（NotDetermined）
    // 时 CoreBluetooth 依然会成功回调，但射频不会真正发包。这里显式上报
    // 权限警告，避免 Rust 侧把「回调成功」误报为「iPhone 可发现」。
    CBManagerAuthorization authorization = [CBManager authorization];
    if (authorization != CBManagerAuthorizationAllowedAlways) {
        [self emitStatus:PBHIDErrorPermissionPending nativeErrorCode:0];
        return;
    }
    [self emitStatus:0 nativeErrorCode:0];
}

- (void)peripheralManager:(CBPeripheralManager *)peripheral
                   central:(CBCentral *)central
 didSubscribeToCharacteristic:(CBCharacteristic *)characteristic {
    dispatch_assert_queue(_queue);
    NSUUID *key = central.identifier;
    _centrals[key] = central;
    NSMutableSet<NSString *> *set = _subscriptions[key];
    if (!set) {
        set = [NSMutableSet set];
        _subscriptions[key] = set;
    }
    [set addObject:PBCharacteristicKey(characteristic)];

    NSData *baseline = nil;
    if (characteristic == _mouseReport) {
        baseline = _cachedReports[@1];
    } else if (characteristic == _bootMouseInput) {
        baseline = [self bootMouseData];
    } else if (characteristic == _keyboardReport || characteristic == _bootKeyboardInput) {
        baseline = _cachedReports[@2];
    }
    if (!baseline) {
        for (NSNumber *number in _reportsByID) {
            if (_reportsByID[number] == characteristic) {
                baseline = _cachedReports[number];
                break;
            }
        }
    }
    if (baseline) {
        [self updateData:baseline forCharacteristic:(CBMutableCharacteristic *)characteristic];
    }
    [self emitStatus:0];
}

- (void)peripheralManager:(CBPeripheralManager *)peripheral
                   central:(CBCentral *)central
didUnsubscribeFromCharacteristic:(CBCharacteristic *)characteristic {
    dispatch_assert_queue(_queue);
    NSUUID *key = central.identifier;
    NSMutableSet<NSString *> *set = _subscriptions[key];
    [set removeObject:PBCharacteristicKey(characteristic)];
    if (set.count == 0) {
        [_subscriptions removeObjectForKey:key];
        [_centrals removeObjectForKey:key];
    }
    [_pendingReports removeObjectForKey:PBCharacteristicKey(characteristic)];
    [self emitStatus:0];
}

- (void)peripheralManagerIsReadyToUpdateSubscribers:(CBPeripheralManager *)peripheral {
    dispatch_assert_queue(_queue);
    [self drainPendingReports];
}

- (void)peripheralManager:(CBPeripheralManager *)peripheral
    didReceiveReadRequest:(CBATTRequest *)request {
    dispatch_assert_queue(_queue);
    NSData *value = [self valueForCharacteristic:request.characteristic];
    if (request.offset > value.length) {
        [peripheral respondToRequest:request withResult:CBATTErrorInvalidOffset];
        return;
    }
    request.value = [value subdataWithRange:NSMakeRange(request.offset, value.length - request.offset)];
    [peripheral respondToRequest:request withResult:CBATTErrorSuccess];
}

- (void)peripheralManager:(CBPeripheralManager *)peripheral
   didReceiveWriteRequests:(NSArray<CBATTRequest *> *)requests {
    dispatch_assert_queue(_queue);
    for (CBATTRequest *request in requests) {
        if ([request.characteristic.UUID isEqual:_keyboardLEDReport.UUID] ||
            [request.characteristic.UUID isEqual:_bootKeyboardOutputUUID]) {
            [self handleOutputWrite:request];
        }
    }
    // CoreBluetooth requires one response for the first request in a batch;
    // responding to every request can make iOS abort the HID setup sequence.
    if (requests.count > 0) {
        [peripheral respondToRequest:requests.firstObject withResult:CBATTErrorSuccess];
    }
}

@end

// macOS CoreBluetooth uses the host computer/GAP name for the device entry
// shown by parts of the system Bluetooth UI. CBPeripheralManager cannot
// replace that system-owned name, so expose it for the pairing instruction
// instead of telling the user to search for an entry that may have another
// name (for example, "Mac mini").
void phonebridge_hid_get_host_name(char *buffer, size_t length) {
    if (!buffer || length == 0) {
        return;
    }
    buffer[0] = '\0';
    NSString *name = [[NSHost currentHost] localizedName];
    if (name.length == 0) {
        name = [[NSProcessInfo processInfo] hostName];
    }
    const char *utf8 = [name UTF8String];
    if (!utf8 || utf8[0] == '\0') {
        return;
    }
    size_t sourceLength = strlen(utf8);
    size_t copyLength = sourceLength < (length - 1) ? sourceLength : (length - 1);
    memcpy(buffer, utf8, copyLength);
    buffer[copyLength] = '\0';
}

void *phonebridge_hid_create(PBHIDStatusCallback callback,
                             void *context,
                             const char *local_name) {
    @autoreleasepool {
        NSString *name = local_name ? [[NSString alloc] initWithUTF8String:local_name] : @"快投屏";
        __block PBHIDPeripheral *peripheral = nil;
        // Construct the manager on the same main queue used for all CoreBluetooth
        // callbacks.  The C ABI can still be called synchronously from Rust.
        PBPerformOnMainQueueSync(^{
            peripheral = [[PBHIDPeripheral alloc] initWithCallback:callback
                                                              context:context
                                                            localName:name];
        });
        return (__bridge_retained void *)peripheral;
    }
}

int32_t phonebridge_hid_start(void *handle) {
    if (!handle) {
        return -1;
    }
    PBHIDPeripheral *peripheral = (__bridge PBHIDPeripheral *)handle;
    return [peripheral start];
}

void phonebridge_hid_stop(void *handle) {
    if (!handle) {
        return;
    }
    PBHIDPeripheral *peripheral = (__bridge PBHIDPeripheral *)handle;
    [peripheral stop];
}

void phonebridge_hid_destroy(void *handle) {
    if (!handle) {
        return;
    }
    PBHIDPeripheral *peripheral = (__bridge_transfer PBHIDPeripheral *)handle;
    (void)peripheral;
}

int32_t phonebridge_hid_send_keyboard(void *handle, const uint8_t *bytes, size_t length) {
    if (!handle || !bytes || length != 8) {
        return -1;
    }
    PBHIDPeripheral *peripheral = (__bridge PBHIDPeripheral *)handle;
    return [peripheral sendKeyboard:PBData(bytes, length)];
}

int32_t phonebridge_hid_send_mouse(void *handle, const uint8_t *bytes, size_t length) {
    if (!handle || !bytes || length != 4) {
        return -1;
    }
    PBHIDPeripheral *peripheral = (__bridge PBHIDPeripheral *)handle;
    return [peripheral sendMouse:PBData(bytes, length)];
}
