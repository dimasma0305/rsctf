export const MAX_PARTICIPATION_REVIEW_ROWS = 50;
export const MAX_PARTICIPATION_REVIEW_PAGE_BYTES = 256 * 1024;
export const MAX_PARTICIPATION_REVIEW_DETAIL_BYTES = 1024 * 1024;

const PARTICIPATION_STATUSES = new Set([
  "Pending",
  "Accepted",
  "Rejected",
  "Suspended",
  "Unsubmitted",
]);
const SUMMARY_KEYS = [
  "divisionId",
  "id",
  "registeredMemberCount",
  "status",
  "teamAvatar",
  "teamId",
  "teamMemberCount",
  "teamName",
];
const DETAIL_KEYS = ["id", "members", "teamAvatar", "teamId", "teamName"];
const MEMBER_KEYS = [
  "avatar",
  "email",
  "isCaptain",
  "isRegistered",
  "phone",
  "realName",
  "stdNumber",
  "userId",
  "userName",
];

const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const nullableString = (value) => value === null || typeof value === "string";
const exactKeys = (value, expected) =>
  object(value) &&
  JSON.stringify(Object.keys(value).sort()) === JSON.stringify(expected);
const positiveInteger = (value) => Number.isSafeInteger(value) && value > 0;
const nonnegativeInteger = (value) => Number.isSafeInteger(value) && value >= 0;

export function participationReviewOperations(
  gameId,
  participationId,
  divisionId,
) {
  if (!positiveInteger(gameId) || !positiveInteger(participationId)) {
    throw new Error("participation review operation ids must be positive integers");
  }
  if (divisionId !== null && !positiveInteger(divisionId)) {
    throw new Error("participation review division id must be null or positive");
  }
  const list = `/api/game/${gameId}/participations`;
  const operations = [
    { id: "page_default", kind: "page", maxRows: 10, path: `${list}?count=10&skip=0` },
    { id: "page_max", kind: "page", maxRows: 50, path: `${list}?count=50&skip=0` },
    { id: "page_tail", kind: "page", maxRows: 10, path: `${list}?count=10&skip=11990` },
    {
      id: "page_status",
      kind: "page",
      maxRows: 10,
      path: `${list}?count=10&skip=0&status=Accepted`,
    },
    {
      id: "page_literal_search",
      kind: "page",
      maxRows: 10,
      path: `${list}?count=10&skip=0&search=${encodeURIComponent("%_")}`,
    },
    {
      id: "detail",
      kind: "detail",
      path: `${list}/${participationId}`,
    },
  ];
  if (divisionId !== null) {
    operations.splice(4, 0, {
      id: "page_division",
      kind: "page",
      maxRows: 10,
      path: `${list}?count=10&skip=0&divisionId=${divisionId}`,
    });
  }
  return Object.freeze(operations.map((operation) => Object.freeze(operation)));
}

export function validParticipationReviewPage(model, bodyBytes, maxRows) {
  if (!object(model) || !Array.isArray(model.data)) return false;
  if (!positiveInteger(maxRows) || maxRows > MAX_PARTICIPATION_REVIEW_ROWS) return false;
  if (!nonnegativeInteger(model.length) || model.length !== model.data.length) return false;
  if (!nonnegativeInteger(model.total) || model.total < model.length) return false;
  if (model.data.length > maxRows) return false;
  if (!nonnegativeInteger(bodyBytes) || bodyBytes > MAX_PARTICIPATION_REVIEW_PAGE_BYTES) return false;

  return model.data.every(
    (row) =>
      exactKeys(row, SUMMARY_KEYS) &&
      positiveInteger(row.id) &&
      positiveInteger(row.teamId) &&
      typeof row.teamName === "string" &&
      nullableString(row.teamAvatar) &&
      nonnegativeInteger(row.registeredMemberCount) &&
      nonnegativeInteger(row.teamMemberCount) &&
      (row.divisionId === null || positiveInteger(row.divisionId)) &&
      PARTICIPATION_STATUSES.has(row.status),
  );
}

export function validParticipationReviewDetail(model, bodyBytes) {
  if (!exactKeys(model, DETAIL_KEYS) || !Array.isArray(model.members)) return false;
  if (!nonnegativeInteger(bodyBytes) || bodyBytes > MAX_PARTICIPATION_REVIEW_DETAIL_BYTES) return false;
  if (!positiveInteger(model.id) || !positiveInteger(model.teamId)) return false;
  if (typeof model.teamName !== "string" || !nullableString(model.teamAvatar)) return false;

  return model.members.every(
    (member) =>
      exactKeys(member, MEMBER_KEYS) &&
      typeof member.userId === "string" &&
      nullableString(member.userName) &&
      nullableString(member.email) &&
      nullableString(member.realName) &&
      nullableString(member.stdNumber) &&
      nullableString(member.phone) &&
      nullableString(member.avatar) &&
      typeof member.isRegistered === "boolean" &&
      typeof member.isCaptain === "boolean",
  );
}

export function validParticipationReviewResponse(operation, model, bodyBytes) {
  if (!object(operation)) return false;
  return operation.kind === "page"
    ? validParticipationReviewPage(model, bodyBytes, operation.maxRows)
    : operation.kind === "detail" && validParticipationReviewDetail(model, bodyBytes);
}
